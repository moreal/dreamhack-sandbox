# CLAUDE.md

## Changelog

Changelog entries are [sacho](https://sacho.dev/) fragments under `changes.d/`
— one file per user-visible change, written for the people who run `wargame`
(no type names, module paths, or how-hard-it-was narration), committed
together with the code it describes. Never edit `CHANGELOG.md` by hand: its
unreleased region is generated, and `sacho release` is what compiles the
fragments into a dated section.

```sh
mise install                     # once: installs the pinned sacho (mise.toml)
sacho add <topic-name>           # new fragment, then write the entry in it
sacho fmt && sacho check         # normalize and validate before committing
```

Config lives in `sacho.toml`. Prefix with `mise exec --` if mise shims are not
on your `PATH`.
