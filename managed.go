package main

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"gopkg.in/yaml.v3"
)

const managedFileMode = 0600

type coreSupervisor struct {
	mu         sync.Mutex
	binary     string
	configFile string
	resumeFile string
	tokenFile  string
	coreListen string
	coreURL    *url.URL
	command    *exec.Cmd
	done       chan struct{}
	startedAt  time.Time
	lastError  string
	stopping   bool
	logs       *coreLogBuffer
	applyMu    sync.Mutex
}

type runtimeStatus struct {
	Managed    bool      `json:"managed"`
	Running    bool      `json:"running"`
	PID        int       `json:"pid,omitempty"`
	StartedAt  time.Time `json:"started_at,omitempty"`
	LastError  string    `json:"last_error,omitempty"`
	CoreURL    string    `json:"core_url"`
	ConfigFile string    `json:"config_file"`
	ResumeFile string    `json:"resume_file"`
}

func newCoreSupervisor(binary, configFile, resumeFile, tokenFile, coreListen string, coreURL *url.URL) *coreSupervisor {
	return &coreSupervisor{
		binary: binary, configFile: configFile, resumeFile: resumeFile,
		tokenFile: tokenFile, coreListen: coreListen, coreURL: coreURL, logs: newCoreLogBuffer(),
	}
}

func (supervisor *coreSupervisor) Prepare() error {
	for _, path := range []string{supervisor.configFile, supervisor.resumeFile, supervisor.tokenFile} {
		if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
			return fmt.Errorf("create managed data directory: %w", err)
		}
	}
	if err := ensureTokenFile(supervisor.tokenFile); err != nil {
		return err
	}
	return normalizeYAMLConfigFile(supervisor.configFile, supervisor.resumeFile, supervisor.tokenFile, supervisor.coreListen)
}

func (supervisor *coreSupervisor) Start() error {
	supervisor.mu.Lock()
	defer supervisor.mu.Unlock()
	supervisor.stopping = false
	return supervisor.startLocked()
}

func (supervisor *coreSupervisor) startLocked() error {
	if supervisor.command != nil {
		return nil
	}
	if err := supervisor.Prepare(); err != nil {
		return err
	}
	command := exec.Command(supervisor.binary, "--config", supervisor.configFile)
	command.Stdout = io.MultiWriter(os.Stdout, supervisor.logs.Writer("stdout"))
	command.Stderr = io.MultiWriter(os.Stderr, supervisor.logs.Writer("stderr"))
	if err := command.Start(); err != nil {
		supervisor.lastError = err.Error()
		return fmt.Errorf("start Core: %w", err)
	}
	done := make(chan struct{})
	supervisor.command = command
	supervisor.done = done
	supervisor.startedAt = time.Now()
	supervisor.lastError = ""
	supervisor.logs.Append("system", fmt.Sprintf("Core started with PID %d", command.Process.Pid))
	go supervisor.wait(command, done)
	return nil
}

func (supervisor *coreSupervisor) wait(command *exec.Cmd, done chan struct{}) {
	err := command.Wait()
	supervisor.mu.Lock()
	restart := false
	if supervisor.command == command {
		supervisor.command = nil
		supervisor.done = nil
		if err != nil {
			supervisor.lastError = err.Error()
		}
		restart = !supervisor.stopping
	}
	if err != nil {
		supervisor.logs.Append("system", "Core exited: "+err.Error())
	} else {
		supervisor.logs.Append("system", "Core exited")
	}
	close(done)
	supervisor.mu.Unlock()
	if restart {
		go supervisor.restartLoop()
	}
}

func (supervisor *coreSupervisor) restartLoop() {
	for {
		time.Sleep(time.Second)
		supervisor.mu.Lock()
		if supervisor.stopping || supervisor.command != nil {
			supervisor.mu.Unlock()
			return
		}
		err := supervisor.startLocked()
		if err != nil {
			supervisor.lastError = err.Error()
			supervisor.logs.Append("system", "Core restart failed: "+err.Error())
		}
		supervisor.mu.Unlock()
		if err == nil {
			return
		}
	}
}

func (supervisor *coreSupervisor) Stop(ctx context.Context) error {
	supervisor.mu.Lock()
	supervisor.stopping = true
	command := supervisor.command
	done := supervisor.done
	supervisor.mu.Unlock()
	if command == nil {
		return nil
	}
	if err := command.Process.Signal(os.Interrupt); err != nil && !errors.Is(err, os.ErrProcessDone) {
		return fmt.Errorf("signal Core: %w", err)
	}
	select {
	case <-done:
		return nil
	case <-ctx.Done():
		if err := command.Process.Kill(); err != nil && !errors.Is(err, os.ErrProcessDone) {
			return fmt.Errorf("kill Core: %w", err)
		}
		<-done
		return ctx.Err()
	}
}

func (supervisor *coreSupervisor) Restart(ctx context.Context) error {
	supervisor.applyMu.Lock()
	defer supervisor.applyMu.Unlock()
	if err := supervisor.Stop(ctx); err != nil {
		return err
	}
	return supervisor.Start()
}

func (supervisor *coreSupervisor) Status() runtimeStatus {
	supervisor.mu.Lock()
	defer supervisor.mu.Unlock()
	status := runtimeStatus{
		Managed: true, LastError: supervisor.lastError, CoreURL: supervisor.coreURL.String(),
		ConfigFile: supervisor.configFile, ResumeFile: supervisor.resumeFile,
	}
	if supervisor.command != nil && supervisor.command.Process != nil {
		status.Running = true
		status.PID = supervisor.command.Process.Pid
		status.StartedAt = supervisor.startedAt
	}
	return status
}

func (supervisor *coreSupervisor) LogsAfter(sequence uint64, limit int) ([]coreLogEntry, uint64) {
	return supervisor.logs.EntriesAfter(sequence, limit)
}

func (supervisor *coreSupervisor) Token() (string, error) {
	data, err := os.ReadFile(supervisor.tokenFile)
	if err != nil {
		return "", fmt.Errorf("read managed token: %w", err)
	}
	return string(bytes.TrimSpace(data)), nil
}

func (supervisor *coreSupervisor) ApplyConfig(ctx context.Context, authorization string, body []byte) (int, http.Header, []byte, error) {
	supervisor.applyMu.Lock()
	defer supervisor.applyMu.Unlock()
	normalizedJSON, normalizedYAML, err := normalizeJSONConfig(body, supervisor.resumeFile, supervisor.tokenFile, supervisor.coreListen)
	if err != nil {
		return http.StatusBadRequest, nil, nil, err
	}
	status, headers, responseBody, err := supervisor.applyCoreConfig(ctx, authorization, normalizedJSON)
	if err != nil {
		return 0, nil, nil, err
	}
	if status >= 200 && status < 300 || coreErrorCode(responseBody) == "RESTART_REQUIRED" {
		if err := atomicWriteFile(supervisor.configFile, normalizedYAML, managedFileMode); err != nil {
			return 0, nil, nil, fmt.Errorf("configuration applied but persistence failed: %w", err)
		}
	}
	if coreErrorCode(responseBody) == "RESTART_REQUIRED" {
		return supervisor.reloadCoreConfig(ctx, authorization)
	}
	return status, headers, responseBody, nil
}

func (supervisor *coreSupervisor) ApplyYAMLConfig(ctx context.Context, authorization string, body []byte) (int, http.Header, []byte, error) {
	supervisor.applyMu.Lock()
	defer supervisor.applyMu.Unlock()
	normalizedYAML, err := normalizeYAMLConfig(body, supervisor.resumeFile, supervisor.tokenFile, supervisor.coreListen)
	if err != nil {
		return http.StatusBadRequest, nil, nil, err
	}
	var config map[string]any
	if err := yaml.Unmarshal(normalizedYAML, &config); err != nil {
		return http.StatusBadRequest, nil, nil, fmt.Errorf("decode normalized YAML configuration: %w", err)
	}
	normalizedJSON, err := json.Marshal(config)
	if err != nil {
		return http.StatusBadRequest, nil, nil, fmt.Errorf("encode configuration JSON: %w", err)
	}
	status, headers, responseBody, err := supervisor.applyCoreConfig(ctx, authorization, normalizedJSON)
	if err != nil {
		return 0, nil, nil, err
	}
	if status >= 200 && status < 300 || coreErrorCode(responseBody) == "RESTART_REQUIRED" {
		if err := atomicWriteFile(supervisor.configFile, normalizedYAML, managedFileMode); err != nil {
			return 0, nil, nil, fmt.Errorf("configuration applied but persistence failed: %w", err)
		}
	}
	if coreErrorCode(responseBody) == "RESTART_REQUIRED" {
		return supervisor.reloadCoreConfig(ctx, authorization)
	}
	return status, headers, responseBody, nil
}

func (supervisor *coreSupervisor) applyCoreConfig(ctx context.Context, authorization string, normalizedJSON []byte) (int, http.Header, []byte, error) {
	endpoint := *supervisor.coreURL
	endpoint.Path = joinURLPath(endpoint.Path, "/api/v1/config")
	request, err := http.NewRequestWithContext(ctx, http.MethodPut, endpoint.String(), bytes.NewReader(normalizedJSON))
	if err != nil {
		return 0, nil, nil, err
	}
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Authorization", authorization)
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		return 0, nil, nil, fmt.Errorf("apply Core configuration: %w", err)
	}
	defer response.Body.Close()
	responseBody, err := io.ReadAll(response.Body)
	if err != nil {
		return 0, nil, nil, fmt.Errorf("read Core configuration response: %w", err)
	}
	return response.StatusCode, response.Header.Clone(), responseBody, nil
}

func (supervisor *coreSupervisor) reloadCoreConfig(ctx context.Context, authorization string) (int, http.Header, []byte, error) {
	endpoint := *supervisor.coreURL
	endpoint.Path = joinURLPath(endpoint.Path, "/api/v1/config/reload")
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint.String(), nil)
	if err != nil {
		return 0, nil, nil, err
	}
	request.Header.Set("Authorization", authorization)
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		return 0, nil, nil, fmt.Errorf("reload persisted Core configuration: %w", err)
	}
	defer response.Body.Close()
	responseBody, err := io.ReadAll(response.Body)
	if err != nil {
		return 0, nil, nil, fmt.Errorf("read Core configuration reload response: %w", err)
	}
	return response.StatusCode, response.Header.Clone(), responseBody, nil
}

func coreErrorCode(body []byte) string {
	var envelope struct {
		Error struct {
			Code string `json:"code"`
		} `json:"error"`
	}
	if json.Unmarshal(body, &envelope) != nil {
		return ""
	}
	return envelope.Error.Code
}

func ensureTokenFile(path string) error {
	if data, err := os.ReadFile(path); err == nil && len(bytes.TrimSpace(data)) > 0 {
		return nil
	} else if err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("read managed token: %w", err)
	}
	random := make([]byte, 32)
	if _, err := rand.Read(random); err != nil {
		return fmt.Errorf("generate managed token: %w", err)
	}
	return atomicWriteFile(path, []byte(hex.EncodeToString(random)+"\n"), managedFileMode)
}

func normalizeYAMLConfigFile(path, resumeFile, tokenFile, coreListen string) error {
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		data = defaultConfig
	} else if err != nil {
		return fmt.Errorf("read managed configuration: %w", err)
	}
	normalized, err := normalizeYAMLConfig(data, resumeFile, tokenFile, coreListen)
	if err != nil {
		return err
	}
	return atomicWriteFile(path, normalized, managedFileMode)
}

func normalizeYAMLConfig(data []byte, resumeFile, tokenFile, coreListen string) ([]byte, error) {
	var document yaml.Node
	if err := yaml.Unmarshal(data, &document); err != nil {
		return nil, fmt.Errorf("decode managed configuration: %w", err)
	}
	if len(document.Content) == 0 {
		document.Content = []*yaml.Node{{Kind: yaml.MappingNode, Tag: "!!map"}}
	}
	managedValues := []struct {
		path  []string
		value string
		tag   string
	}{
		{[]string{"version"}, "1", "!!int"},
		{[]string{"control", "rest", "enabled"}, "true", "!!bool"},
		{[]string{"control", "rest", "listen"}, coreListen, "!!str"},
		{[]string{"control", "rest", "secret"}, "", "!!str"},
		{[]string{"control", "rest", "secret-file"}, tokenFile, "!!str"},
		{[]string{"state", "resume-file"}, resumeFile, "!!str"},
	}
	for _, managed := range managedValues {
		if err := setYAMLScalar(&document, managed.path, managed.value, managed.tag); err != nil {
			return nil, fmt.Errorf("normalize managed configuration: %w", err)
		}
	}
	normalized, err := yaml.Marshal(&document)
	if err != nil {
		return nil, fmt.Errorf("encode managed configuration: %w", err)
	}
	return normalized, nil
}

func setYAMLScalar(document *yaml.Node, path []string, value, tag string) error {
	if len(path) == 0 {
		return fmt.Errorf("empty YAML path")
	}
	current := document
	if current.Kind == yaml.DocumentNode {
		if len(current.Content) == 0 {
			current.Content = []*yaml.Node{{Kind: yaml.MappingNode, Tag: "!!map"}}
		}
		current = current.Content[0]
	}
	for index, key := range path {
		if current.Kind != yaml.MappingNode {
			return fmt.Errorf("%s is not a mapping", strings.Join(path[:index], "."))
		}
		var child *yaml.Node
		for contentIndex := 0; contentIndex+1 < len(current.Content); contentIndex += 2 {
			if current.Content[contentIndex].Value == key {
				child = current.Content[contentIndex+1]
				break
			}
		}
		if child == nil {
			keyNode := &yaml.Node{Kind: yaml.ScalarNode, Tag: "!!str", Value: key}
			child = &yaml.Node{}
			current.Content = append(current.Content, keyNode, child)
		}
		if index == len(path)-1 {
			child.Kind = yaml.ScalarNode
			child.Tag = tag
			child.Value = value
			child.Content = nil
			return nil
		}
		if child.Kind != yaml.MappingNode {
			child.Kind = yaml.MappingNode
			child.Tag = "!!map"
			child.Value = ""
			child.Content = nil
		}
		current = child
	}
	return nil
}

func normalizeJSONConfig(data []byte, resumeFile, tokenFile, coreListen string) ([]byte, []byte, error) {
	var config map[string]any
	decoder := json.NewDecoder(bytes.NewReader(data))
	if err := decoder.Decode(&config); err != nil {
		return nil, nil, fmt.Errorf("decode configuration: %w", err)
	}
	if config == nil {
		return nil, nil, fmt.Errorf("configuration must be an object")
	}
	normalizeManagedFields(config, resumeFile, tokenFile, coreListen)
	normalizedJSON, err := json.Marshal(config)
	if err != nil {
		return nil, nil, fmt.Errorf("encode configuration JSON: %w", err)
	}
	normalizedYAML, err := yaml.Marshal(config)
	if err != nil {
		return nil, nil, fmt.Errorf("encode configuration YAML: %w", err)
	}
	return normalizedJSON, normalizedYAML, nil
}

func normalizeManagedFields(config map[string]any, resumeFile, tokenFile, coreListen string) {
	config["version"] = 1
	control := childMap(config, "control")
	rest := childMap(control, "rest")
	rest["enabled"] = true
	rest["listen"] = coreListen
	delete(rest, "secret")
	rest["secret-file"] = tokenFile
	state := childMap(config, "state")
	state["resume-file"] = resumeFile
}

func childMap(parent map[string]any, key string) map[string]any {
	if child, ok := parent[key].(map[string]any); ok {
		return child
	}
	child := make(map[string]any)
	parent[key] = child
	return child
}

func atomicWriteFile(path string, data []byte, defaultMode os.FileMode) error {
	directory := filepath.Dir(path)
	if err := os.MkdirAll(directory, 0700); err != nil {
		return err
	}
	mode := defaultMode
	if info, err := os.Stat(path); err == nil {
		mode = info.Mode().Perm()
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	temporary, err := os.CreateTemp(directory, "."+filepath.Base(path)+".tmp-*")
	if err != nil {
		return err
	}
	temporaryPath := temporary.Name()
	defer os.Remove(temporaryPath)
	if err := temporary.Chmod(mode); err != nil {
		temporary.Close()
		return err
	}
	if _, err := temporary.Write(data); err != nil {
		temporary.Close()
		return err
	}
	if err := temporary.Sync(); err != nil {
		temporary.Close()
		return err
	}
	if err := temporary.Close(); err != nil {
		return err
	}
	return os.Rename(temporaryPath, path)
}

func joinURLPath(base, path string) string {
	return strings.TrimRight(base, "/") + "/" + strings.TrimLeft(path, "/")
}
