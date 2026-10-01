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
cargo install --path . --locked
```

## Usage

```
$ binit --help

  Usage
    $ binit <path|glob> […]
    $ binit --files-from <file>

  Options
    --files-from    Read more paths from a file, one per line; - is stdin
    --verbose, -v   Print each item trashed
    --dry-run, -n   Show what would be trashed; change nothing
    --quiet, -q     Suppress the summary line
    --json          Machine-readable output
    --no-color      Disable color

  Examples
    $ binit unicorn.png rainbow.png
    $ binit *.log
    $ binit --dry-run build\
    $ binit --files-from list.txt
```

Windows limits a command line to 32,767 characters. For a longer list, put the
paths in a file and pass `--files-from`. It is UTF-8, one literal path per line
(no glob expansion), blank lines ignored, and its paths come after any given on
the command line. `--files-from -` reads stdin; binit never reads stdin
otherwise.

`-r`, `-R` and `--recursive` are accepted and do nothing, so `binit -rf build\`
works from muscle memory. `-f` has one effect: a path that does not exist is
reported as skipped (`"reason": "missing"`) instead of failing, as with
`rm -f`, so cleanup scripts can be rerun. It forces nothing. Every other
failure still fails, and `--force` is an error.

A drive root (`C:\`, `C:`, or a subst drive that points at one) is refused.

Exit codes: `0` all recycled or skipped · `1` one or more items failed · `2`
invalid usage · `3` the shell could not be initialized.

With `--json`, `skipped[].reason` is `duplicate`, `nested_in` or `missing`
(`container` is `null` for `missing`). `failed[].code` is one of
`NOT_RECYCLABLE`, `UNC_NO_RECYCLE_BIN`, `SUBST_CYCLE`, `NOT_FOUND`,
`ACCESS_DENIED`, `IN_USE`, `PATH_TOO_LONG`, `DRIVE_ROOT`, `EMPTY_PATH`,
`SHELL_ERROR`, `NO_RESULT` or `COM_INIT_FAILED`.

## Credit

The recycling guarantee is a port of
[sindresorhus/recycle-bin](https://github.com/sindresorhus/recycle-bin).

## License

MIT
