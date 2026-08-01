package app

import (
	"bytes"
	"sync"
	"time"
)

const (
	maxCoreLogLines   = 2000
	maxCoreLogPending = 64 * 1024
)

type coreLogEntry struct {
	Sequence  uint64    `json:"sequence"`
	Timestamp time.Time `json:"timestamp"`
	Stream    string    `json:"stream"`
	Message   string    `json:"message"`
}

type coreLogBuffer struct {
	mu      sync.Mutex
	next    uint64
	entries []coreLogEntry
}

func newCoreLogBuffer() *coreLogBuffer {
	return &coreLogBuffer{entries: make([]coreLogEntry, 0, maxCoreLogLines)}
}

func (buffer *coreLogBuffer) Append(stream, message string) {
	buffer.mu.Lock()
	defer buffer.mu.Unlock()
	buffer.next++
	buffer.entries = append(buffer.entries, coreLogEntry{
		Sequence: buffer.next, Timestamp: time.Now(), Stream: stream, Message: message,
	})
	if overflow := len(buffer.entries) - maxCoreLogLines; overflow > 0 {
		copy(buffer.entries, buffer.entries[overflow:])
		buffer.entries = buffer.entries[:maxCoreLogLines]
	}
}

func (buffer *coreLogBuffer) EntriesAfter(sequence uint64, limit int) ([]coreLogEntry, uint64) {
	buffer.mu.Lock()
	defer buffer.mu.Unlock()
	start := 0
	for start < len(buffer.entries) && buffer.entries[start].Sequence <= sequence {
		start++
	}
	if limit > 0 && len(buffer.entries)-start > limit {
		start = len(buffer.entries) - limit
	}
	entries := append([]coreLogEntry(nil), buffer.entries[start:]...)
	next := sequence
	if len(entries) > 0 {
		next = entries[len(entries)-1].Sequence
	} else if buffer.next > next {
		next = buffer.next
	}
	return entries, next
}

func (buffer *coreLogBuffer) Writer(stream string) *coreLineWriter {
	return &coreLineWriter{buffer: buffer, stream: stream}
}

type coreLineWriter struct {
	mu      sync.Mutex
	buffer  *coreLogBuffer
	stream  string
	pending []byte
}

func (writer *coreLineWriter) Write(data []byte) (int, error) {
	writer.mu.Lock()
	defer writer.mu.Unlock()
	writer.pending = append(writer.pending, data...)
	for {
		newline := bytes.IndexByte(writer.pending, '\n')
		if newline < 0 {
			break
		}
		writer.appendLine(writer.pending[:newline])
		writer.pending = writer.pending[newline+1:]
	}
	if len(writer.pending) > maxCoreLogPending {
		writer.appendLine(writer.pending[:maxCoreLogPending])
		writer.pending = writer.pending[maxCoreLogPending:]
	}
	return len(data), nil
}

func (writer *coreLineWriter) appendLine(line []byte) {
	line = bytes.TrimSuffix(line, []byte{'\r'})
	writer.buffer.Append(writer.stream, string(line))
}
