# git

Activates inside a Git work tree (`**/.git`) when `git` is on `PATH`.

- Prompt segment `{git}`: current branch, plus `*` when the tree is dirty.
- Aliases: `gs`, `gl`, `ga`, `gc`, `gd`, `gds`, `gp`, `gpl`, `gb`, `gsw`, and
  from `init.rushy`: `gco`, `gcb`, `gst`, `gstp`, `grb`.
- Completes `git` subcommands.
- Hook: shows `git status --short` when you enter a repository. Set
  `bundles.allow_hooks = false` to turn hooks off.
