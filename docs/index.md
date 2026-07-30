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
  - title: Two containers
    details: Metric runs as one application container with MongoDB. The web interface is included.
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

Metric 0.1.0 is an early release. Review the [known limits](known-limits.md)
before using it for important production data.

## What you need

- Docker with Docker Compose;
- a machine that can keep its MongoDB data;
- HTTPS when Metric is available over the internet.

Start with [Install Metric](getting-started.md). Most users do not need to change
the advanced settings.
