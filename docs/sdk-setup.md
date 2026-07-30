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
npm install @sentry/browser
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
npm install @sentry/node
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
pip install sentry-sdk
```

```python
import sentry_sdk

sentry_sdk.init(dsn="YOUR_METRIC_DSN", traces_sample_rate=0)
sentry_sdk.capture_message("Metric test event")
```

## Java

Use the official `io.sentry:sentry` package:

```java
import io.sentry.Sentry;

Sentry.init(options -> {
    options.setDsn("YOUR_METRIC_DSN");
    options.setTracesSampleRate(0.0);
});

Sentry.captureMessage("Metric test event");
```

## .NET

Use the official `Sentry` package:

```csharp
using Sentry;

SentrySdk.Init(options =>
{
    options.Dsn = "YOUR_METRIC_DSN";
    options.TracesSampleRate = 0;
});

SentrySdk.CaptureMessage("Metric test event");
```

## Check the result

Open **Issues** after sending the test event. Processing is asynchronous, so the
first issue may take a short moment to appear.

See [SDK compatibility](compatibility.md) for versions verified by the Metric
test suite.
