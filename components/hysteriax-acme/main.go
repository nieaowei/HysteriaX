package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/signal"
	"syscall"
	"time"
)

func main() {
	if e := execute(); e != nil {
		fmt.Fprintln(os.Stderr, e)
		os.Exit(1)
	}
}
func execute() error {
	if len(os.Args) == 2 && os.Args[1] == "version" {
		fmt.Println(version)
		return nil
	}
	if len(os.Args) != 3 {
		return fmt.Errorf("usage: hysteriax-acme check|ensure|run|status CONFIG.json")
	}
	command := os.Args[1]
	if command != "check" && command != "ensure" && command != "run" && command != "status" {
		return fmt.Errorf("unknown command")
	}
	c, e := loadConfig(os.Args[2])
	if e != nil {
		return e
	}
	if command == "check" {
		return nil
	}
	if command == "status" {
		return json.NewEncoder(os.Stdout).Encode(c.status())
	}
	if e = c.ensureDirs(); e != nil {
		return e
	}
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGTERM, syscall.SIGINT)
	defer stop()
	create := func(ctx context.Context) (issuer, error) { return c.issuer(ctx) }
	if command == "ensure" {
		limited, cancel := context.WithTimeout(ctx, 175*time.Second)
		defer cancel()
		return c.maintain(limited, true, create)
	}
	for {
		limited, cancel := context.WithTimeout(ctx, 175*time.Second)
		e = c.maintain(limited, false, create)
		cancel()
		if e != nil {
			fmt.Fprintln(os.Stderr, "certificate maintenance deferred; see status")
		}
		delay := time.Hour
		s := c.status()
		if s.NextAttempt != nil {
			delay = time.Until(*s.NextAttempt)
			if delay < time.Second {
				delay = time.Second
			}
			if delay > time.Hour {
				delay = time.Hour
			}
		}
		timer := time.NewTimer(delay)
		select {
		case <-ctx.Done():
			timer.Stop()
			return nil
		case <-timer.C:
		}
	}
}
