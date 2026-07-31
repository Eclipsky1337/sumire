package main

import (
	"context"
	"embed"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"log"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"syscall"
	"time"
)

//go:embed web/*
var webFiles embed.FS

//go:embed default-config.yaml
var defaultConfig []byte

func main() {
	listen := flag.String("listen", "127.0.0.1:9080", "WebUI listen address")
	coreAddress := flag.String("core", "http://127.0.0.1:9090", "ZJU Portal Core REST address")
	coreBinary := flag.String("core-binary", "", "managed Core executable (default: next to WebUI)")
	externalCore := flag.Bool("external-core", false, "use an externally managed Core")
	dataDirectory := flag.String("data-dir", "", "managed Core data directory (default: next to WebUI)")
	configFile := flag.String("config", "", "managed Core configuration file (default: <data-dir>/config.yaml)")
	resumeFile := flag.String("resume-state", "", "managed Core Resume State file (default: <data-dir>/resume-state.json)")
	coreListen := flag.String("core-listen", "127.0.0.1:9090", "managed Core REST listen address")
	coreLogConsole := flag.Bool("core-log-console", false, "mirror managed Core stdout/stderr to the terminal")
	flag.Parse()

	executablePath, err := os.Executable()
	if err != nil {
		log.Fatalf("resolve WebUI executable: %v", err)
	}
	resolvedCoreBinary, managed, err := resolveCoreBinary(*coreBinary, *externalCore, executablePath, runtime.GOOS)
	if err != nil {
		log.Fatal(err)
	}
	if !managed && !*externalCore {
		log.Printf("bundled Core was not found; using external Core at %s", *coreAddress)
	}

	var supervisor *coreSupervisor
	if managed {
		if !isLoopbackListen(*listen) {
			log.Fatal("managed mode requires -listen to use a loopback address")
		}
		if *dataDirectory == "" {
			*dataDirectory = filepath.Join(filepath.Dir(executablePath), "data")
		}
		managedPaths, err := resolveManagedPaths(*dataDirectory, *configFile, *resumeFile)
		if err != nil {
			log.Fatal(err)
		}
		*coreAddress = "http://" + *coreListen
		target, parseErr := url.Parse(*coreAddress)
		if parseErr != nil {
			log.Fatal(parseErr)
		}
		supervisor = newCoreSupervisor(resolvedCoreBinary, managedPaths.config, managedPaths.resume, managedPaths.token, *coreListen, target)
		supervisor.SetConsoleLogging(*coreLogConsole)
		if err := supervisor.Prepare(); err != nil {
			log.Fatal(err)
		}
		if err := supervisor.Start(); err != nil {
			log.Fatal(err)
		}
	}

	target, err := url.Parse(*coreAddress)
	if err != nil || (target.Scheme != "http" && target.Scheme != "https") || target.Host == "" {
		fmt.Fprintf(os.Stderr, "invalid -core address %q\n", *coreAddress)
		os.Exit(2)
	}

	assets, err := fs.Sub(webFiles, "web")
	if err != nil {
		log.Fatal(err)
	}
	systemProxy := newSystemProxyController()
	handler := newHandler(target, assets, supervisor, systemProxy)

	server := &http.Server{
		Addr:              *listen,
		Handler:           handler,
		ReadHeaderTimeout: 5 * time.Second,
		IdleTimeout:       60 * time.Second,
	}

	ctx, stopSignals := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stopSignals()
	go func() {
		<-ctx.Done()
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 8*time.Second)
		defer cancel()
		_ = server.Shutdown(shutdownCtx)
	}()

	log.Printf("Sumire listening on http://%s (managed: %t)", *listen, supervisor != nil)
	if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		log.Fatal(err)
	}
	proxyCtx, cancelProxy := context.WithTimeout(context.Background(), 8*time.Second)
	if err := systemProxy.Close(proxyCtx); err != nil {
		log.Printf("disable system proxy: %v", err)
	}
	cancelProxy()
	if supervisor != nil {
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 8*time.Second)
		defer cancel()
		if err := supervisor.Stop(shutdownCtx); err != nil && !errors.Is(err, context.Canceled) {
			log.Printf("stop managed Core: %v", err)
		}
	}
}

func newHandler(target *url.URL, assets fs.FS, supervisor *coreSupervisor, systemProxy *systemProxyController) http.Handler {
	indexHTML, indexErr := fs.ReadFile(assets, "index.html")
	proxy := httputil.NewSingleHostReverseProxy(target)
	originalDirector := proxy.Director
	proxy.Director = func(request *http.Request) {
		originalDirector(request)
		request.Header.Del("Origin")
		request.Header.Del("Referer")
		request.Host = target.Host
	}
	proxy.FlushInterval = -1
	proxy.ErrorHandler = func(writer http.ResponseWriter, request *http.Request, proxyErr error) {
		log.Printf("proxy %s %s: %v", request.Method, request.URL.Path, proxyErr)
		http.Error(writer, "Core is unavailable", http.StatusBadGateway)
	}

	static := http.FileServer(http.FS(assets))
	mux := http.NewServeMux()
	mux.HandleFunc("/healthz", func(writer http.ResponseWriter, _ *http.Request) {
		writer.Header().Set("Content-Type", "text/plain; charset=utf-8")
		_, _ = writer.Write([]byte("ok\n"))
	})
	mux.HandleFunc("/webui/runtime", func(writer http.ResponseWriter, request *http.Request) {
		if request.Method != http.MethodGet {
			methodNotAllowed(writer, http.MethodGet)
			return
		}
		if supervisor == nil {
			writeWebJSON(writer, http.StatusOK, map[string]any{"result": runtimeStatus{Managed: false, CoreURL: target.String()}})
			return
		}
		writeWebJSON(writer, http.StatusOK, map[string]any{"result": supervisor.Status()})
	})
	mux.HandleFunc("/webui/bootstrap", func(writer http.ResponseWriter, request *http.Request) {
		if request.Method != http.MethodGet {
			methodNotAllowed(writer, http.MethodGet)
			return
		}
		if supervisor == nil {
			writeWebJSON(writer, http.StatusOK, map[string]any{"result": map[string]any{"managed": false}})
			return
		}
		token, err := supervisor.Token()
		if err != nil {
			writeWebError(writer, http.StatusInternalServerError, "MANAGED_TOKEN_UNAVAILABLE", err.Error())
			return
		}
		writeWebJSON(writer, http.StatusOK, map[string]any{"result": map[string]any{"managed": true, "token": token}})
	})
	mux.HandleFunc("/webui/config", func(writer http.ResponseWriter, request *http.Request) {
		if supervisor == nil {
			writeWebError(writer, http.StatusConflict, "CORE_NOT_MANAGED", "Core is not managed by WebUI")
			return
		}
		switch request.Method {
		case http.MethodGet:
			data, err := os.ReadFile(supervisor.configFile)
			if err != nil {
				writeWebError(writer, http.StatusInternalServerError, "CONFIG_UNAVAILABLE", err.Error())
				return
			}
			writer.Header().Set("Content-Type", "application/yaml; charset=utf-8")
			_, _ = writer.Write(data)
		case http.MethodPut:
			request.Body = http.MaxBytesReader(writer, request.Body, 4<<20)
			body, err := io.ReadAll(request.Body)
			if err != nil {
				writeWebError(writer, http.StatusBadRequest, "CONFIG_INVALID", err.Error())
				return
			}
			status, headers, responseBody, err := supervisor.ApplyYAMLConfig(request.Context(), request.Header.Get("Authorization"), body)
			if err != nil {
				if status == 0 {
					status = http.StatusInternalServerError
				}
				writeWebError(writer, status, "CONFIG_APPLY_FAILED", err.Error())
				return
			}
			writeUpstreamResponse(writer, status, headers, responseBody)
		default:
			methodNotAllowed(writer, http.MethodGet, http.MethodPut)
		}
	})
	mux.HandleFunc("/webui/restart", func(writer http.ResponseWriter, request *http.Request) {
		if request.Method != http.MethodPost {
			methodNotAllowed(writer, http.MethodPost)
			return
		}
		if supervisor == nil {
			writeWebError(writer, http.StatusConflict, "CORE_NOT_MANAGED", "Core is not managed by WebUI")
			return
		}
		restartCtx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
		defer cancel()
		if err := supervisor.Restart(restartCtx); err != nil {
			writeWebError(writer, http.StatusInternalServerError, "CORE_RESTART_FAILED", err.Error())
			return
		}
		writeWebJSON(writer, http.StatusOK, map[string]any{"result": supervisor.Status()})
	})
	mux.HandleFunc("/webui/system-proxy", func(writer http.ResponseWriter, request *http.Request) {
		if supervisor == nil {
			writeWebError(writer, http.StatusConflict, "CORE_NOT_MANAGED", "system proxy is only available in managed mode")
			return
		}
		if !supervisor.Authorized(request.Header.Get("Authorization")) {
			writeWebError(writer, http.StatusUnauthorized, "UNAUTHORIZED", "invalid managed Core token")
			return
		}
		switch request.Method {
		case http.MethodGet:
			writeWebJSON(writer, http.StatusOK, map[string]any{"result": systemProxy.Status()})
		case http.MethodPut:
			var params struct {
				Enabled      bool   `json:"enabled"`
				HTTPAddress  string `json:"http_address"`
				SOCKSAddress string `json:"socks_address"`
			}
			request.Body = http.MaxBytesReader(writer, request.Body, 1<<20)
			decoder := json.NewDecoder(request.Body)
			decoder.DisallowUnknownFields()
			if err := decoder.Decode(&params); err != nil {
				writeWebError(writer, http.StatusBadRequest, "SYSTEM_PROXY_INVALID", err.Error())
				return
			}
			if params.Enabled {
				if strings.TrimSpace(params.HTTPAddress) == "" && strings.TrimSpace(params.SOCKSAddress) == "" {
					writeWebError(writer, http.StatusBadRequest, "SYSTEM_PROXY_INVALID", "at least one proxy address is required")
					return
				}
				for label, address := range map[string]string{"HTTP": params.HTTPAddress, "SOCKS": params.SOCKSAddress} {
					if strings.TrimSpace(address) == "" {
						continue
					}
					if _, err := parseSystemProxyEndpoint(address); err != nil {
						writeWebError(writer, http.StatusBadRequest, "SYSTEM_PROXY_INVALID", fmt.Sprintf("invalid %s proxy address: %v", label, err))
						return
					}
				}
			}
			proxyCtx, cancel := context.WithTimeout(request.Context(), 15*time.Second)
			defer cancel()
			state, err := systemProxy.Configure(proxyCtx, params.Enabled, params.HTTPAddress, params.SOCKSAddress)
			if err != nil {
				status := http.StatusInternalServerError
				if !state.Supported {
					status = http.StatusNotImplemented
				}
				writeWebError(writer, status, "SYSTEM_PROXY_FAILED", err.Error())
				return
			}
			writeWebJSON(writer, http.StatusOK, map[string]any{"result": state})
		default:
			methodNotAllowed(writer, http.MethodGet, http.MethodPut)
		}
	})
	mux.HandleFunc("/webui/routing", func(writer http.ResponseWriter, request *http.Request) {
		if request.Method != http.MethodPut {
			methodNotAllowed(writer, http.MethodPut)
			return
		}
		if supervisor == nil {
			writeWebError(writer, http.StatusConflict, "CORE_NOT_MANAGED", "Core is not managed by WebUI")
			return
		}
		var params struct {
			Mode string `json:"mode"`
		}
		request.Body = http.MaxBytesReader(writer, request.Body, 4<<20)
		decoder := json.NewDecoder(request.Body)
		decoder.DisallowUnknownFields()
		if err := decoder.Decode(&params); err != nil {
			writeWebError(writer, http.StatusBadRequest, "CONFIG_INVALID", err.Error())
			return
		}
		status, headers, responseBody, err := supervisor.UpdateRoutingMode(request.Context(), request.Header.Get("Authorization"), params.Mode)
		if err != nil {
			if status == 0 {
				status = http.StatusInternalServerError
			}
			writeWebError(writer, status, "CONFIG_APPLY_FAILED", err.Error())
			return
		}
		writeUpstreamResponse(writer, status, headers, responseBody)
	})
	mux.HandleFunc("/webui/logs", func(writer http.ResponseWriter, request *http.Request) {
		if request.Method != http.MethodGet {
			methodNotAllowed(writer, http.MethodGet)
			return
		}
		if supervisor == nil {
			writeWebError(writer, http.StatusConflict, "CORE_NOT_MANAGED", "Core is not managed by WebUI")
			return
		}
		var after uint64
		if value := request.URL.Query().Get("after"); value != "" {
			parsed, err := strconv.ParseUint(value, 10, 64)
			if err != nil {
				writeWebError(writer, http.StatusBadRequest, "INVALID_LOG_CURSOR", "invalid log cursor")
				return
			}
			after = parsed
		}
		entries, next := supervisor.LogsAfter(after, 500)
		writeWebJSON(writer, http.StatusOK, map[string]any{"result": map[string]any{"entries": entries, "next": next}})
	})
	mux.HandleFunc("/api/v1/config", func(writer http.ResponseWriter, request *http.Request) {
		if supervisor == nil || request.Method != http.MethodPut {
			proxy.ServeHTTP(writer, request)
			return
		}
		request.Body = http.MaxBytesReader(writer, request.Body, 4<<20)
		body, err := io.ReadAll(request.Body)
		if err != nil {
			writeWebError(writer, http.StatusBadRequest, "CONFIG_INVALID", err.Error())
			return
		}
		status, headers, responseBody, err := supervisor.ApplyConfig(request.Context(), request.Header.Get("Authorization"), body)
		if err != nil {
			writeWebError(writer, http.StatusInternalServerError, "CONFIG_PERSIST_FAILED", err.Error())
			return
		}
		writeUpstreamResponse(writer, status, headers, responseBody)
	})
	mux.HandleFunc("/api/v1/config/reload", func(writer http.ResponseWriter, request *http.Request) {
		if supervisor != nil && request.Method == http.MethodPost {
			if err := supervisor.Prepare(); err != nil {
				writeWebError(writer, http.StatusInternalServerError, "CONFIG_PREPARE_FAILED", err.Error())
				return
			}
		}
		proxy.ServeHTTP(writer, request)
	})
	mux.Handle("/api/", proxy)
	mux.HandleFunc("/", func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path != "/" && strings.Contains(request.URL.Path, ".") {
			static.ServeHTTP(writer, request)
			return
		}
		if indexErr != nil {
			http.Error(writer, "WebUI assets are unavailable", http.StatusInternalServerError)
			return
		}
		writer.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = writer.Write(indexHTML)
	})
	return securityHeaders(mux)
}

type managedPaths struct {
	config string
	resume string
	token  string
}

func resolveManagedPaths(dataDirectory, configFile, resumeFile string) (managedPaths, error) {
	absoluteData, err := filepath.Abs(dataDirectory)
	if err != nil {
		return managedPaths{}, fmt.Errorf("resolve data directory: %w", err)
	}
	if configFile == "" {
		configFile = filepath.Join(absoluteData, "config.yaml")
	}
	if resumeFile == "" {
		resumeFile = filepath.Join(absoluteData, "resume-state.json")
	}
	absoluteConfig, err := filepath.Abs(configFile)
	if err != nil {
		return managedPaths{}, fmt.Errorf("resolve configuration path: %w", err)
	}
	absoluteResume, err := filepath.Abs(resumeFile)
	if err != nil {
		return managedPaths{}, fmt.Errorf("resolve Resume State path: %w", err)
	}
	return managedPaths{config: absoluteConfig, resume: absoluteResume, token: filepath.Join(absoluteData, "control.token")}, nil
}

func resolveCoreBinary(explicit string, external bool, executablePath, operatingSystem string) (string, bool, error) {
	if external {
		if explicit != "" {
			return "", false, fmt.Errorf("-core-binary and -external-core cannot be used together")
		}
		return "", false, nil
	}
	if explicit != "" {
		resolved, err := exec.LookPath(explicit)
		if err != nil {
			return "", false, fmt.Errorf("find Core executable %q: %w", explicit, err)
		}
		absolute, err := filepath.Abs(resolved)
		if err != nil {
			return "", false, fmt.Errorf("resolve Core executable: %w", err)
		}
		return absolute, true, nil
	}
	candidate := filepath.Join(filepath.Dir(executablePath), coreExecutableName(operatingSystem))
	info, err := os.Stat(candidate)
	if errors.Is(err, os.ErrNotExist) {
		return "", false, nil
	}
	if err != nil {
		return "", false, fmt.Errorf("inspect bundled Core: %w", err)
	}
	if !info.Mode().IsRegular() {
		return "", false, fmt.Errorf("bundled Core %q is not a regular file", candidate)
	}
	if operatingSystem != "windows" && info.Mode().Perm()&0111 == 0 {
		return "", false, fmt.Errorf("bundled Core %q is not executable", candidate)
	}
	return candidate, true, nil
}

func coreExecutableName(operatingSystem string) string {
	if operatingSystem == "windows" {
		return "zju-portal-core.exe"
	}
	return "zju-portal-core"
}

func isLoopbackListen(address string) bool {
	host, _, err := net.SplitHostPort(address)
	if err != nil {
		return false
	}
	if host == "localhost" {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

func methodNotAllowed(writer http.ResponseWriter, methods ...string) {
	writer.Header().Set("Allow", strings.Join(methods, ", "))
	writeWebError(writer, http.StatusMethodNotAllowed, "METHOD_NOT_ALLOWED", "method not allowed")
}

func writeWebError(writer http.ResponseWriter, status int, code, message string) {
	writeWebJSON(writer, status, map[string]any{"error": map[string]any{"code": code, "message": message, "retryable": false}})
}

func writeWebJSON(writer http.ResponseWriter, status int, value any) {
	writer.Header().Set("Content-Type", "application/json")
	writer.WriteHeader(status)
	_ = json.NewEncoder(writer).Encode(value)
}

func writeUpstreamResponse(writer http.ResponseWriter, status int, headers http.Header, body []byte) {
	for name, values := range headers {
		for _, value := range values {
			writer.Header().Add(name, value)
		}
	}
	writer.WriteHeader(status)
	_, _ = writer.Write(body)
}

func securityHeaders(next http.Handler) http.Handler {
	return http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		writer.Header().Set("X-Content-Type-Options", "nosniff")
		writer.Header().Set("X-Frame-Options", "DENY")
		writer.Header().Set("Referrer-Policy", "no-referrer")
		writer.Header().Set("Content-Security-Policy", "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'")
		next.ServeHTTP(writer, request)
	})
}
