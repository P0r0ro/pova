# Development

```sh
./scripts/check.sh                      # rustfmt check, clippy (warnings are errors), all tests
cargo test -- --ignored live_api        # also hits the real YouTube Music API
```

The tests cover the cavacore port with real test tones, every btop theme on
your machine, the queue, the search box, likes, the fast-start resolver, kitty
image output, and rendering at five terminal sizes.
[MILESTONES.md](../MILESTONES.md) shows how the project came together.

The architecture is described in [how-it-works.md](how-it-works.md).

Back to the [README](../README.md).
