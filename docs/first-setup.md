# First setup

The first setup creates the owner account and the first organization.

## Create the owner

1. Open Metric in your browser.
2. Select **First setup**.
3. Paste the `METRIC_BOOTSTRAP_TOKEN` from the container logs.
4. Enter your name and email.
5. Choose a password with at least 12 characters.
6. Enter an organization name.
7. Select **Create owner and organization**.

Metric shows the organization ID after setup. Save it: the sign-in form asks for
the email, password and organization ID.

## Create the first project

After signing in:

1. Select **Create your first project**.
2. Enter the application or service name, for example `Payments API`.
3. Keep **HMAC pseudonymization** unless you need another IP-address policy.
4. On Medium or High, keep the signal types you need. On Min or Low, begin with
   Error Events and add sampled logs or traces only after watching disk use.
5. Leave Session Replay disabled until you have reviewed its privacy settings.
6. Create the project.

Metric creates the first DSN for the project and opens the SDK setup page.

## What is a DSN?

A DSN tells an SDK where to send events and which Metric project owns them. It is
not a personal login token.

Copy the DSN from **Connect an SDK** and continue with the
[SDK guide](sdk-setup.md).
