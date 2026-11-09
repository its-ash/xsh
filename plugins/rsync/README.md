# rsync

Activates when `rsync` is on `PATH`.

- `rsync-copy SRC DEST`: archive copy with progress.
- `rsync-move SRC DEST`: copy, then delete the source files.
- `rsync-update SRC DEST`: copy only newer files.
- `rsync-synchronize SRC DEST`: mirror, deleting extra files in `DEST`.
  Add `--dry-run` first to preview.
