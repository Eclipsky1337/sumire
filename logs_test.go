package main

import "testing"

func TestCoreLineWriterCollectsCompleteLines(t *testing.T) {
	buffer := newCoreLogBuffer()
	writer := buffer.Writer("stderr")
	_, _ = writer.Write([]byte("first"))
	_, _ = writer.Write([]byte(" line\nsecond line\r\npartial"))

	entries, cursor := buffer.EntriesAfter(0, 100)
	if len(entries) != 2 || entries[0].Message != "first line" || entries[1].Message != "second line" {
		t.Fatalf("entries = %#v", entries)
	}
	if entries[0].Stream != "stderr" || cursor != entries[1].Sequence {
		t.Fatalf("stream/cursor = %q/%d", entries[0].Stream, cursor)
	}

	_, _ = writer.Write([]byte(" end\n"))
	entries, _ = buffer.EntriesAfter(cursor, 100)
	if len(entries) != 1 || entries[0].Message != "partial end" {
		t.Fatalf("partial entry = %#v", entries)
	}
}

func TestCoreLogBufferLimitsHistory(t *testing.T) {
	buffer := newCoreLogBuffer()
	for index := 0; index < maxCoreLogLines+25; index++ {
		buffer.Append("stdout", "line")
	}
	entries, next := buffer.EntriesAfter(0, maxCoreLogLines+100)
	if len(entries) != maxCoreLogLines {
		t.Fatalf("entry count = %d", len(entries))
	}
	if entries[0].Sequence != 26 || next != maxCoreLogLines+25 {
		t.Fatalf("sequence range = %d..%d", entries[0].Sequence, next)
	}
}
