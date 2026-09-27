package main

import (
	"bufio"
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"os"
	"os/signal"
	"syscall"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/server"
)

func main() {
	if len(os.Args) == 2 && (os.Args[1] == "version" || os.Args[1] == "--version") {
		fmt.Fprintln(os.Stdout, config.EngineVersion)
		return
	}
	if len(os.Args) == 2 && os.Args[1] == "hash-password" {
		if err := hashPassword(os.Stdin, os.Stdout, os.Stderr); err != nil {
			log.Fatalf("hash-password: %v", err)
		}
		return
	}
	cfg, err := parseConfig("graphite-meter", os.Args[1:], os.Stderr)
	if errors.Is(err, flag.ErrHelp) {
		return
	}
	if err != nil {
		log.Fatalf("configuration error: %q", err)
	}
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	if err := server.Run(ctx, new(cfg)); err != nil {
		log.Fatalf("server error: %q", err)
	}
}

func parseConfig(name string, args []string, usage io.Writer) (config.Config, error) {
	cfg, err := config.Load()
	if err != nil {
		return config.Config{}, err
	}
	fs := flag.NewFlagSet(name, flag.ContinueOnError)
	fs.SetOutput(usage)
	config.RegisterFlags(fs, &cfg)
	if err := fs.Parse(args); err != nil {
		return config.Config{}, err
	}
	if err := cfg.Validate(); err != nil {
		return config.Config{}, err
	}
	return cfg, nil
}

func hashPassword(stdin *os.File, out, prompts io.Writer) error {
	in := bufio.NewReader(stdin)
	fmt.Fprint(prompts, "Password: ")
	first, err := auth.ReadPassword(in, stdin)
	if err != nil {
		return err
	}
	fmt.Fprint(prompts, "Confirm password: ")
	second, err := auth.ReadPassword(in, stdin)
	if err != nil {
		return err
	}
	if first != second {
		return errors.New("passwords do not match")
	}
	encoded, err := auth.HashPassword(first)
	if err != nil {
		return err
	}
	fmt.Fprintln(out, encoded)
	return nil
}
