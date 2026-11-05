// Package logx writes server log lines to stdout and an append-only file.
package logx

import (
	"fmt"
	"os"
	"path/filepath"
	"sync"
)

type Logger struct {
	mu   sync.Mutex
	file *os.File
	name string
}

// New opens logs/<name>.log (relative to the working directory).
func New(name string) (*Logger, error) {
	if err := os.MkdirAll("logs", 0o755); err != nil {
		return nil, err
	}
	file, err := os.OpenFile(filepath.Join("logs", name+".log"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return nil, err
	}
	return &Logger{file: file, name: name}, nil
}

func (l *Logger) write(line string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	fmt.Println(line)
	if l.file != nil {
		fmt.Fprintln(l.file, line)
	}
}

func (l *Logger) Printf(format string, args ...any) { l.write(fmt.Sprintf(format, args...)) }

// Tagged returns a view that prefixes every line, used per connection so
// concurrent sessions stay distinguishable in the shared log.
func (l *Logger) Tagged(tag string) *Tag { return &Tag{logger: l, tag: tag} }

type Tag struct {
	logger *Logger
	tag    string
}

func (t *Tag) SetTag(tag string) { t.tag = tag }
func (t *Tag) Tag() string       { return t.tag }

func (t *Tag) Printf(format string, args ...any) {
	line := fmt.Sprintf(format, args...)
	if t.tag != "" {
		line = "[" + t.tag + "] " + line
	}
	t.logger.write(line)
}
