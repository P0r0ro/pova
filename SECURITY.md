# Security

## Reporting a vulnerability

Please **don't open a public issue** for security problems. Report them
privately instead: on GitHub, go to the repository's **Security** tab and
click **Report a vulnerability**. You'll get a reply as soon as possible, and
a fix will be released before the details are made public.

## What counts

Anything that could expose a user's data or let someone else control pova,
for example:

- the YouTube Music login cookie (`~/.config/pova/cookie.txt`) leaking, being
  logged, or being sent anywhere other than YouTube Music;
- the control socket (`$XDG_RUNTIME_DIR/pova/control.sock`) being reachable
  by other users;
- running unexpected commands through mpv, yt-dlp or the packages' install
  scripts.

## Supported versions

Fixes go into the latest release. Please update to it before reporting.
