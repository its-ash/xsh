# Manual test checklist

These checks need a real terminal. Automated tests cannot cover them because
CI and sandboxes do not give test processes a PTY or a keyboard. Run them on
macOS before a release, and write down the terminal you used.

Use a throw-away profile so your real settings stay safe:

```sh
export XSH_CONFIG_DIR=$(mktemp -d) XSH_DATA_DIR=$(mktemp -d)
export XSH_HISTORY__KEY_SOURCE=passphrase XSH_HISTORY_PASSPHRASE=test
```

## 1. Prompt and line editing

- [ ] `xsh` shows `user path ❯ ` (no host name). After `false`, the next prompt shows `[1]`.
- [ ] Arrow keys, `Home`, `End`, `Ctrl-A`, `Ctrl-E`, and `Ctrl-W` edit the line.
- [ ] `echo one \` then `Enter` shows the `> ` continuation prompt. Type `two`: it prints `one two`.
- [ ] `echo 'open` then `Enter` shows `> `. Close the quote on the next line: both lines print.
- [ ] `Tab` completes commands, bundle subcommands (in a Git repository, `git ch<Tab>`), and file names. A name with a space is inserted as `a\ b`.
- [ ] `Ctrl-C` at the prompt clears the line and gives a new prompt. `Ctrl-D` on an empty line exits.

## 2. Running commands and signals

- [ ] `sleep 30` then `Ctrl-C`: `sleep` stops and the prompt returns; xsh stays alive.
- [ ] `sleep 30` in one terminal; in another, `kill -TERM <xsh pid>`: `sleep` ends, and xsh exits with status 143 if no command was running.
- [ ] `vim` (or `less`) works normally: arrow keys, resize, and exit.
- [ ] `sleep 5 &` prints `[1] <pid>`. `jobs` lists it. After it ends, the next prompt shows `[1] Done (0)  sleep 5 &`.
- [ ] `vim`, then `Ctrl-Z`: the prompt comes back with `[1]+  Stopped    vim`. `jobs` shows `Stopped`. `fg` opens vim again. `sleep 30`, `Ctrl-Z`, `bg`: it runs in the background.
- [ ] `x=1; sh -c 'echo [$x]'` prints `[]`. `export x; sh -c 'echo [$x]'` prints `[1]`. `export -n x` hides it again.
- [ ] `export A=1`, `echo $A ${A} $? $$`, `unset A`.
- [ ] `echo hi | wc -c` prints `3`; `echo a > f; echo b >> f; wc -l < f` prints `2`.
- [ ] With `shell.capture_stderr = true` (the default), `git clone https://github.com/git/git /tmp/g` shows live progress (`Receiving objects: 12% ...`) on one updating line, the same as in Terminal.app. `sh -c '[ -t 2 ] && echo tty'` prints `tty`. `ls /nope`, then `ai fix`: the error text is still sent.

## 3. History and `Ctrl-R`

- [ ] Run `echo alpha`, `echo beta`, `ls`. Press `Ctrl-R`: a list opens under the prompt.
- [ ] Type `ealp`: `echo alpha` is first. `Enter` puts it in the prompt (it does not run).
- [ ] `Esc` closes the list and the prompt is intact (no leftover rows).
- [ ] A line that starts with a space is not in `history`.
- [ ] `history pause`, run `echo secret`, `history resume`: `history` does not list `echo secret`.
- [ ] Restart xsh: `Ctrl-R` still finds `echo alpha`.

## 3b. Option suggestions from man pages

- [ ] Run `tar --version`, then type `tar -` and press `Tab`: a list of tar options with short descriptions appears.
- [ ] Type `tar --ext` and `Tab`: it completes to `--extract`.
- [ ] In a Git repository run `git commit --help` (or `git status`), then `git commit --` and `Tab`: options of `git commit` appear.
- [ ] A command with no man page (`xsh-nothing -`): `Tab` does nothing harmful and does not hang (at most 2 seconds the first time).
- [ ] Without `man` on `PATH` (`PATH=/bin xsh` on a system where `man` is elsewhere): `Tab` still completes file names.

## 4. Palette, themes, accessibility

- [ ] `Ctrl-K` opens the palette. Type `mono` and `Enter`: the buffer holds `theme mono`.
- [ ] `theme tokyo-night` changes colors; `theme mono` removes them.
- [ ] Edit `$XSH_CONFIG_DIR/themes/mine.toml` (copy `tokyo-night`, change `accent`). `theme mine` works.
- [ ] While xsh runs, edit the file again: the next prompt prints `configuration reloaded` and the colors change. No restart.
- [ ] Run five ordinary commands (`ls`, `pwd`, ...): `configuration reloaded` does **not** appear (history writes must not trigger it).
- [ ] `NO_COLOR=1 xsh`: no color codes in the prompt (`xsh 2>&1 | cat -v`).
- [ ] `xsh --screen-reader`: the prompt ends with `>`, not `❯`; `xsh new` shows `+`, `-`, `|` borders and `[active]` on the focused pane.

## 5. Bundles and prompt segments

- [ ] In a Git repository: `bundle install ./plugins/git`. The prompt shows ` main` (and `*` when the tree is dirty). `gs` runs `git status --short --branch`.
- [ ] `cd /tmp`: the segment and `gs` are gone.
- [ ] Resize the window narrower than the prompt: low-priority segments (`node`, `python`) disappear first, then the path is shortened.

## 6. AI

Use a local model (`ollama`) or a real key in `$XSH_CONFIG_DIR/ai.toml`.

- [ ] Type `list the 5 biggest files here`, press `Ctrl-Space`: a command and a one-line explanation appear. `Enter` inserts it; `r` runs it; `c` cancels.
- [ ] The same with a request such as `delete everything under /`: xsh prints `refused: ...` and does not offer to run it.
- [ ] Type `find the largest log file and show its last 5 lines`, press `Ctrl-Shift-Space` (or `Alt-G`): the agent asks before each command. `n` skips, `a` aborts, and it stops after 10 steps.
- [ ] In `xsh.app`, run an agent goal: a pane opens to the right and runs each approved command. The left pane shows the model's text and the questions, and keeps the focus. When the agent stops, the right pane is a normal shell. Close the right pane during a run: the agent stops at the next step.
- [ ] In `xsh.app`, split a tab: each pane has a close button, and a click on either pane lets you type in it.
- [ ] Type a goal and press `Alt-A`: the agent runs its commands without a question. A destructive command still shows `refused: ...`.
- [ ] During an agent run, press `Ctrl-C` in each state: while it waits for the model, at a question, during a command in the left pane, and during a command in the right pane. Each time the agent stops and the prompt comes back.
- [ ] `ai explain tar -xzf a.tgz` prints a short explanation.
- [ ] Run `cargo bild` (a failing command), then `ai fix`: the answer mentions the typo.
- [ ] `ls $XSH_DATA_DIR/ai`: only `.bin` files and `salt`; `strings` on a `.bin` file shows no prompt text.

## 7. Multiplexer

Prefix is `Ctrl-b` unless you set `mux.prefix`.

- [ ] `xsh new work` opens a pane with a status bar (session, window, pane, bundles, AI, clock).
- [ ] `Ctrl-b %` splits side by side; `Ctrl-b "` splits top and bottom. The focused pane has the accent border.
- [ ] `Ctrl-b h/j/k/l` and `Ctrl-b` + arrows move focus.
- [ ] `Ctrl-b Ctrl-←/→/↑/↓` (or `Alt` + arrow) moves the divider.
- [ ] `Ctrl-b c` opens a window; `Ctrl-b n` and `Ctrl-b p` switch.
- [ ] `Ctrl-b x` closes the focused pane; closing the last pane ends the session.
- [ ] `Ctrl-b d` detaches. `xsh ls` lists `work`. A long command (`sleep 60; echo done`) keeps running.
- [ ] `xsh attach work` shows the old screen and the running command.
- [ ] Resize the terminal while attached: every pane resizes (run `stty size` in each).
- [ ] `Ctrl-b Ctrl-b` sends a literal `Ctrl-b` to the program.
- [ ] `xsh kill-session work` ends it; `xsh ls` prints `no sessions`.

## 8. macOS notes to record

| Item | Result |
| --- | --- |
| Terminal used | |
| `Ctrl-Space` works | |
| `Ctrl-Shift-Space` works (kitty protocol) | |
| Keychain asks for permission | |
| Hot reload (native watcher or polling) | |

## 9. The app (xsh.app)

Build with `make run`. Use a throw-away profile as described above.

- [ ] A new window shows the `xsh` prompt. `echo $TERM_PROGRAM` prints `xsh`, and `brew` or another tool from your login `PATH` is found.
- [ ] `vim`, `htop`, and `less` draw correctly. Colors match the theme. `printf '\e[38;2;255;100;0mX\n'` shows true color.
- [ ] `⌘T` opens a tab in the same folder. `⌘⇧]` and `⌘⇧[` switch tabs. The `+` button of the tab bar works.
- [ ] `⌘D` and `⌘⇧D` split. `⌘]`, `⌘[`, and `⌘⌥` + arrows move focus. The pane without focus is dim. `stty size` is right in each pane, also after a window resize.
- [ ] `⌘W` closes a pane, then the tab. `exit` closes the pane. With `sleep 100` running, `⌘W` asks first.
- [ ] `⌘F` finds text in the scrollback. `⌘G` goes to the next match.
- [ ] `⌘C`, `⌘V`, and `⌘A` work. A paste with two lines asks first at a plain prompt. `⌘K` clears the scrollback.
- [ ] `⌘=`, `⌘-`, and `⌘0` change the text size in all panes.
- [ ] Drop a file on a pane: its quoted path is typed. `⌘`-click on a URL opens it.
- [ ] Quit and open again: the tabs, splits, and folders come back.
- [ ] `⌘,` opens Settings. On each page, change a value: the window changes at once, and `config.toml` has the new value. Search finds a setting.
- [ ] Edit `config.toml` by hand: open windows change within two seconds. Save a bad value: Settings shows the error.
- [ ] Change a shortcut in Settings: the menu shows it. Set a global hotkey: it hides and shows the app from another app.
- [ ] Opacity below 100%, blur, each title bar style, and the tab bar `Always` all draw correctly.
- [ ] Add a profile with `/bin/zsh`: Shell > New Tab with Profile opens it with its theme and font.
- [ ] Save an AI key: it is in Keychain Access under `xsh-openai`, and `ai.toml` only names the entry. Remove it: the entry is gone.
- [ ] General > Make xsh the Default: double-click a `.command` file, and open `ssh://localhost`. Finder > Services > New xsh Tab Here opens the folder.
