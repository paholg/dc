# Shared settings and shell bootstrap for every tape.
#
# `Source demos/lib/setup.tape` as the first command of a tape, after its `Output`.

Set Shell "bash"
Set FontFamily "FiraCode Nerd Font Mono"
# The prompt ends with `❯` (demos/starship.toml), not VHS's default `>`. VHS
# strips trailing whitespace from the line it matches, so don't include any.
Set WaitPattern /❯/
Set FontSize 24
Set Width 1200
Set Height 650
Set Padding 20
Set TypingSpeed 40ms

# Replace VHS's shell with one configured for recording: `dc`, completions, a
# short prompt, the demo config, and the example project as the working
# directory. `tape.bashrc` ends with `clear`, so the recording starts empty.
# The path is relative to where vhs runs, which `record.sh` makes the repo root.
Hide
Type `exec bash --rcfile demos/tape.bashrc`
Enter
# Let the shell start and the prompt draw before the first visible keystroke.
Sleep 2s
Show
