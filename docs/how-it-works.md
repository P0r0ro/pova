# How pova works

The short version: pova is the conductor, and a few specialists do the actual
work.

```
 keys, mouse ──► App ◄──────────────── events ──────────────────┐
                 │  one central state, redrawn up to 60 fps     │
                 ├──► player ──► mpv (JSON IPC socket) ─────────┤
                 ├──► ytm ─────► YouTube Music (ytmapi-rs) ─────┤
                 ├──► resolver ► yt-dlp, ahead of time ─────────┤
                 ├──► lyrics ──► LRCLIB ────────────────────────┤
                 └──► art ─────► thumbnails, disk cache ────────┘

 mpv's PipeWire stream ─► pw-record ─► capture thread
        ─(lock-free ring buffer)─► DSP thread (cavacore port) ─► bars on screen
```

Two more sit at the edge: the remote-control socket (`pova next`) and media
controls (MPRIS on the D-Bus session bus, for desktop media widgets and media
keys). Both only turn requests into the same commands and hand them to App;
neither talks to mpv itself. Media controls get a snapshot from App only when
what they show changes, never per frame.

Each specialist runs on its own and only talks through messages. A slow lyrics
server can't freeze the visualizer, and a YouTube hiccup can't stop the music.

**Playback.** pova starts mpv with no window and steers it over mpv's JSON
socket: load, pause, seek, volume. pova owns the queue. mpv only ever knows
about two songs, the one playing and the one coming next. Preloading the next
one is what makes playback gapless. Shuffle can be undone, too: every queue
entry remembers when it was added, so turning shuffle off restores your order.

**Crossfade.** The smooth fade is just mpv's `volume-gain`, a gain stage
separate from your volume, moved as the song nears its end. The overlap
crossfade needs two songs playing at once, so for each song change pova
starts a short-lived second mpv, paused at the overlap point, from the song's
already-resolved stream. When the main mpv reaches that point the helper
takes over the ending and fades it out, and the main mpv moves on to the next
song and fades it in. The helper quits a few seconds later. The main mpv stays
in charge of everything (queue, time, lyrics, visualizer); it only ever gets
an ordinary "next".

**The four-second problem.** Before mpv can play a YouTube Music song, yt-dlp
has to work out the real audio address, which involves solving a JavaScript
puzzle YouTube sets. That takes three to four seconds every time. pova's
resolver does it in the background for the songs you're probably about to
play. It runs at most two lookups at a time so YouTube stays happy, and it
keeps the answers until shortly before they expire (about six hours). If a
saved address stops working, pova quietly falls back to the slow way, so
nothing ever fails because of the shortcut.

**YouTube Music.** There's no official API, so pova uses
[ytmapi-rs](https://github.com/nick42d/youtui), which talks to the same
private API the website uses. For misspelled searches, YouTube answers with
"Did you mean", which the library reads as zero results, so pova pulls the
corrected query out of the raw response and searches again.

**The visualizer.** mpv plays through PipeWire under a name unique to each
pova window (`pova-<pid>`). pova finds that stream in the PipeWire graph and
records it with `pw-record`, so you see pova's music and nothing else. Run two
pova windows and each one shows its own song. Samples go through a lock-free
ring buffer to a DSP thread running the cavacore port: a long FFT for bass and
a short one for treble, log-spaced bars, gravity, and automatic sensitivity.
While paused or silent the DSP thread idles, so pova sits at about 2% CPU.

**Lyrics.** On every new song pova asks LRCLIB for time-synced lyrics, first
by exact title, artist and length, then with a looser search. Titles get
cleaned up first: "(Official Video)", "[HD]" and friends are removed. If LRCLIB
has nothing, YouTube Music's plain lyrics are next. If that's empty too, the
cat comes out.

**Album art in kitty.** Images use kitty's graphics protocol with Unicode
placeholders, so each image is sent to the terminal only once, not on every
frame. One catch: an image row only knows its position from its first cell.
When a popup covers the start of a row, kitty forgets where the rest of it
goes. pova fixes the cells next to the popup so the art stays intact around
menus. For video thumbnails, black letterbox bars are trimmed before
cropping to a square.

**Like status.** When you sign in, pova loads your liked songs in the
background, and any list that comes with like information (albums, playlists,
history) adds to it. That's how ♥ shows up even on plain search results, and
how `f` knows to unlike rather than like again.

**Logs** go to `~/.cache/pova/pova.log` and never onto your screen. Use
`pova --log-level debug` to see things like "track started in 343 ms
(pre-resolved URL)".

## Known limits

- pova reads only your first ~400 liked songs. Past that, the YouTube Music
  library pova uses can't parse a page of large libraries, so pova keeps what
  it got rather than showing nothing.
- It all depends on YouTube Music's private API and on yt-dlp keeping up with
  YouTube. When YouTube changes something, update yt-dlp first (`yt-dlp -U` or
  your package manager).
- Linux with PipeWire only, for now. The visualizer needs `pw-record`.

Back to the [README](../README.md).
