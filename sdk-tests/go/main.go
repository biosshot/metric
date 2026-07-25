package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"time"

	"github.com/getsentry/sentry-go"
)

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "Metric DSN argument is required")
		os.Exit(2)
	}
	deadline := time.AfterFunc(15*time.Second, func() {
		fmt.Fprintln(os.Stderr, "real Go SDK sender exceeded its 15 second process deadline")
		os.Exit(2)
	})
	defer deadline.Stop()

	if err := sentry.Init(sentry.ClientOptions{
		Dsn:              os.Args[1],
		Environment:      "sdk-compatibility",
		Release:          "metric-go-sdk-test@1.0.0",
		EnableTracing:    false,
		TracesSampleRate: 0,
		SendDefaultPII:   false,
	}); err != nil {
		fmt.Fprintf(os.Stderr, "Sentry initialization failed: %v\n", err)
		os.Exit(2)
	}
	sentry.ConfigureScope(func(scope *sentry.Scope) {
		scope.SetTag("metric.sdk_test", "go")
	})

	eventID := sentry.CaptureException(errors.New("Metric real Go SDK compatibility event"))
	flushed := sentry.Flush(8 * time.Second)
	if !flushed || eventID == nil {
		fmt.Fprintln(os.Stderr, "the real Go SDK did not flush the captured Event")
		os.Exit(2)
	}
	if err := json.NewEncoder(os.Stdout).Encode(map[string]any{
		"event_id": string(*eventID),
		"flushed":  flushed,
	}); err != nil {
		fmt.Fprintf(os.Stderr, "could not encode result: %v\n", err)
		os.Exit(2)
	}
}
