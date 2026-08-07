# Security Policy

[English](SECURITY.md) | [日本語](SECURITY.ja.md)

## Reporting a vulnerability

Report it privately through GitHub: open the [Security tab](https://github.com/coni524/screen-memory/security) and choose **Report a vulnerability**. That opens a private thread visible only to you and me.

Please do not open a public issue or a pull request for a security problem. Issues are public from the moment you post them.

A report is easiest to act on when it says which version you ran, which mode (AWS or local only), and what an attacker gains. A proof of concept helps but is not required.

I maintain this project alone, in my spare time. I will send a first reply within 7 days. If a fix is warranted, I will publish it as a new release and credit you in the advisory unless you prefer otherwise.

## Supported versions

Only the latest release receives fixes. Older versions are not patched, so please reproduce the problem on the latest release before reporting.

## Your captured data stays with you

Screen Memory stores screenshots and OCR text either on your own machine or in an S3 bucket in the AWS account you deployed into. There is no third-party server and no telemetry, and I have no access to any of it.

So a vulnerability report never needs to contain your own captures or reports. If a screenshot is the clearest way to show the problem, please redact it first.
