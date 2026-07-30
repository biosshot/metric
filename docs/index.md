---
layout: home

hero:
  name: Metric
  text: See production failures clearly
  tagline: A small, self-hosted error tracking service that works with official Sentry SDKs.
  actions:
    - theme: brand
      text: Install with Docker
      link: /getting-started
    - theme: alt
      text: Connect an SDK
      link: /sdk-setup

features:
  - title: Starts at 1 GiB
    details: Choose Min, Low, Medium or High. Small profiles omit Symbolicator and bound memory, disk and retention.
  - title: Keep your SDK
    details: Use the official Sentry SDK for your language and point its DSN to Metric.
  - title: Keep your data
    details: Events, logs, traces and attachments stay in storage that you control.
---

# What is Metric?

Metric receives errors and other signals from your applications, groups repeated
errors into issues and shows the result in a web interface.

Use Metric when you want the Sentry workflow without running the full self-hosted
Sentry stack.

Metric 0.1.1 is an early release. Review the [known limits](known-limits.md)
before using it for important production data.

## What you need

- Docker with Docker Compose;
- a 64-bit machine starting at 1 vCPU, 1 GiB RAM and 15 GiB SSD;
- HTTPS is strongly recommended when Metric is available over the internet.

Start with [Install Metric](getting-started.md) and choose a resource profile.
Most users do not need to change its individual settings.
