# Connect an SDK

Metric works with official Sentry SDKs. You normally only need to replace the
Sentry DSN with the DSN shown by Metric.

## Get the DSN

1. Sign in to Metric.
2. Select a project.
3. Open **Connect an SDK**.
4. Copy an active DSN.

It looks like this:

```text
https://PROJECT_KEY@metric.example.com/PROJECT_ID
```

## JavaScript in a browser

```bash
npm install @sentry/browser@10.66.0
```

```javascript
import * as Sentry from "@sentry/browser";

Sentry.init({
  dsn: "YOUR_METRIC_DSN",
  tracesSampleRate: 0
});

Sentry.captureException(new Error("Metric test event"));
```

## Node.js

```bash
npm install @sentry/node@10.66.0
```

```javascript
import * as Sentry from "@sentry/node";

Sentry.init({
  dsn: "YOUR_METRIC_DSN",
  tracesSampleRate: 0
});

Sentry.captureException(new Error("Metric test event"));
```

## Python

```bash
pip install sentry-sdk==2.32.0
```

```python
import sentry_sdk

sentry_sdk.init(dsn="YOUR_METRIC_DSN", traces_sample_rate=0)
sentry_sdk.capture_message("Metric test event")
```

## Java

Use the official `sentry-java` package version 8.50.1. Its Gradle coordinate is
`io.sentry:sentry`:

```kotlin
implementation("io.sentry:sentry:8.50.1")
```

```java
import io.sentry.Sentry;

Sentry.init(options -> {
    options.setDsn("YOUR_METRIC_DSN");
    options.setTracesSampleRate(0.0);
});

Sentry.captureMessage("Metric test event");
```

## .NET

Install the tested `Sentry` package:

```bash
dotnet add package Sentry --version 6.7.0
```

```csharp
using Sentry;

SentrySdk.Init(options =>
{
    options.Dsn = "YOUR_METRIC_DSN";
    options.TracesSampleRate = 0;
});

SentrySdk.CaptureMessage("Metric test event");
```

## Go

```bash
go get github.com/getsentry/sentry-go@v0.48.0
```

```go
package main

import (
    "time"

    "github.com/getsentry/sentry-go"
)

func main() {
    if err := sentry.Init(sentry.ClientOptions{
        Dsn: "YOUR_METRIC_DSN",
    }); err != nil {
        panic(err)
    }

    sentry.CaptureMessage("Metric test event")
    sentry.Flush(2 * time.Second)
}
```

## Rust

```bash
cargo add sentry@0.48.5
```

```rust
fn main() {
    let _guard = sentry::init("YOUR_METRIC_DSN");
    sentry::capture_message("Metric test event", sentry::Level::Error);
}
```

## Check the result

Open **Issues** after sending the test event. Processing is asynchronous, so the
first issue may take a short moment to appear.

See [SDK compatibility](compatibility.md) for versions verified by the Metric
test suite.
