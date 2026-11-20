# Signing in

Search, playback, radio and lyrics all work without an account. Sign in to
get your library, liked songs, history, and the ♥ next to songs you've liked.

pova signs in the same way your browser does: with your YouTube Music
cookies. There's no OAuth app and no password prompt. You just let pova borrow
the cookie your browser already has.

### The easy way

If you're signed in to <https://music.youtube.com> in your browser:

```sh
./scripts/import-cookie.sh chrome     # or firefox, chromium, brave, edge, vivaldi, opera
```

It asks yt-dlp to read your browser's cookie store (on KDE, Chrome's cookies
are unlocked through KWallet), keeps only the youtube.com cookies, and writes
them as a single line to `~/.config/pova/cookie.txt` with permissions `600`.
It never prints the cookie values. To use a specific browser profile, write
`chrome:Profile 2`.

### The manual way

1. Open <https://music.youtube.com> in your browser while signed in.
2. Open DevTools, go to **Network**, and click any request to `music.youtube.com`.
3. Copy the full value of the **Cookie** request header.
4. Paste it on one line into `~/.config/pova/cookie.txt`, then run
   `chmod 600 ~/.config/pova/cookie.txt`.

Restart pova. The bottom of the player box says **● signed in**.

**Treat `cookie.txt` like a password.** It grants access to your Google
account. It stays on your machine and pova only sends it to YouTube Music. If
pova ever goes back to "○ signed out", Google has rotated the cookie, so run
the script again.

Back to the [README](../README.md).
