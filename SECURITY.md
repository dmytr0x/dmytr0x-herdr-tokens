# Security

Collector commands execute with the current user's permissions; they are not sandboxed. Only configure commands and repository scripts you trust. See [Directories and trust](README.md#directories-and-trust) for the execution model.

Do not publish credentials, token values, private repository contents, or raw child output in issues.

Once private vulnerability reporting is enabled on the GitHub repository, use **Security → Report a vulnerability** for sensitive findings. If that option is unavailable, open an issue asking for a private reporting channel without including exploit details or sensitive data.

Include the affected version, platform, impact, and a minimal sanitized reproduction in the private report. Fixes target the latest development version; older-version backports are not guaranteed.
