# Paths (`std.path`)

`std.path` works on paths held as text. Every function reads the text only: none touches the file system, so none follows a symbolic link or checks that a path exists. Paths are `Str`, and results that are part of the path are views into it ([core-semantics.md §5](../core-semantics.md#5-references-and-views-c5)).

```kara
import std.path.{parent, file_name, extension, join, normalize};

let p = "logs/2026/app.log";
let dir = parent(p) ?? "";                  // "logs/2026", a view into p
let ext = extension(p);                     // Some("log")
let out = join(dir, "app.1.log");           // "logs/2026/app.1.log", an owned String
let clean = normalize("logs/./2026/../x");  // "logs/x"
```

The functions are imported by name. `import std.path;` followed by `path.parent(p)` is not supported yet.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `parent` | `fn parent(p: Str) -> Option[Str]` | The path without its last component: `"a/b/c.txt"` gives `"a/b"`, `"a"` gives `""`, `"/a"` gives `"/"`. `None` for `"/"` and `""`. Trailing separators are ignored |
| `file_name` | `fn file_name(p: Str) -> Option[Str]` | The last component: `"a/b/c.txt"` gives `"c.txt"`. `None` when there is none, or when it is `.` or `..`. Trailing separators are ignored |
| `extension` | `fn extension(p: Str) -> Option[Str]` | The file name after its last `.`: `"c.tar.gz"` gives `"gz"`, `"f."` gives `""`. `None` without a file name, without a `.`, or with only a leading one (`".bashrc"`) |
| `stem` | `fn stem(p: Str) -> Option[Str]` | The file name before its last `.`: `"c.tar.gz"` gives `"c.tar"`; `".bashrc"` gives `".bashrc"`. `None` without a file name |
| `join` | `fn join(a: Str, b: Str) -> String` | `b` after `a`, with one separator between them. See the warnings below |
| `normalize` | `fn normalize(p: Str) -> String` | Removes `.` and repeated separators, and lets each `..` take away the component before it: `"a/./b/../c"` gives `"a/c"`. A leading `..` of a relative path stays, a `..` at the root is dropped, and an empty result is `"."`. See the warnings below |
| `is_absolute` | `fn is_absolute(p: Str) -> bool` | Whether the path starts at the root |
| `components` | `fn components(p: Str) -> Vec[Str]` | The components, each a view: `"/usr//lib/./x"` gives `"/"`, `"usr"`, `"lib"`, `"x"`. Empty components and `.` are dropped, except a `.` that starts a relative path; `..` is kept |

## Warnings

- **`join` does not confine.** An absolute `b` replaces `a` (`join("data", "/etc/passwd")` is `"/etc/passwd"`), and a `..` in `b` is kept (`join("data", "../../x")` is `"data/../../x"`). To keep a user-supplied name inside a directory, check that it is not absolute and that `components` of it holds no `..` before joining.
- **`normalize` is lexical.** It does not resolve symbolic links. When `a` is a link, `a/..` on disk is the parent of the link's target, not the directory holding `a`, so `normalize("a/../b")` can name a different file than the file system would.
- **Paths are UTF-8.** A file name that is not valid UTF-8 cannot be held in a `Str`; paths as raw bytes are deferred ([deferred.md](../deferred.md)).

## Separators and platforms

The separator is `/`, and a path is absolute when it starts with `/`. On Windows, `\` is to be accepted as a separator as well, `join` to write `\`, and a drive (`C:\`) or UNC (`\\server\share\`) prefix to be a root. **Not yet implemented**: it needs `#[cfg(target_os: ...)]` ([design.md §4](../design.md#cfg)), which is not implemented yet, so every platform has the `/` behaviour today.
