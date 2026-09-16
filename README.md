# binit

> Move files and folders to the Windows Recycle Bin

In contrast to `Remove-Item` and `del`, which permanently destroy files, this
only moves them to the Recycle Bin, which is much safer and reversible.

binit cannot delete a file permanently. Where other Windows trash tools
silently fall back to permanent deletion — network shares, volumes with the
bin disabled, files over quota — binit refuses and tells you the `Remove-Item`
command to run yourself. There is no `--force`.

Accepts paths and glob patterns. Windows 8+.

## Install

```sh
cargo build --release
```

Then put `target\release\binit.exe` on your PATH.

## Usage

```
$ binit --help

  Usage
    $ binit <path|glob> […]

  Options
    --verbose, -v   Print each item trashed
    --dry-run, -n   Show what would be trashed; change nothing
    --quiet, -q     Suppress the summary line
    --json          Machine-readable output
    --no-color      Disable color

  Examples
    $ binit unicorn.png rainbow.png
    $ binit *.log
    $ binit --dry-run build\
```

Exit codes: `0` all recycled · `1` one or more items failed · `2` invalid
usage · `3` the shell could not be initialized.

## Credit

The recycling guarantee is a port of
[sindresorhus/recycle-bin](https://github.com/sindresorhus/recycle-bin).

## License

MIT
