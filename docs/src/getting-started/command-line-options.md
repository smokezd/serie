# Command Line Options

## \<REVSPEC\>...

Revisions to render.

If not specified, all commits reachable from branches, remote branches, tags and stashes are
rendered, which is the default behaviour.

The arguments are passed to `git log` as they are, so anything it accepts works:

```
$ serie main              # only the commits reachable from main
$ serie main my-feature   # the commits reachable from either branch
$ serie head              # the commits reachable from HEAD
$ serie main..my-feature  # only what my-feature adds on top of main
$ serie main -- README.md # only the commits that touch README.md
```

`head` is accepted in any case and is passed to git as `HEAD`, since a lower case `head` resolves
only on case-insensitive file systems. It is rewritten wherever it names a revision, so `head~2`,
`^head` and `head~3..head` all work as well.

Options of `git log` itself need to be separated with `--`, because `serie` parses its own options
first:

```
$ serie -- --first-parent main
```

When a revspec is given:

- Stashes are not rendered, and refs that point outside the rendered commits are not listed.
- The revspec is shown in the status line, so that a scoped view is not mistaken for the whole
  repository.
- A merge commit whose second parent is not rendered (which ranges such as `main..my-feature` can
  produce) is drawn without its second edge.

## -n, --max-count \<NUMBER\>

Maximum number of commits to render.

If not specified, all commits will be rendered.
It behaves similarly to the `--max-count` option of `git log`.

## -p, --protocol \<TYPE\>

A protocol type for rendering images of commit graphs.

_Possible values:_ `auto`, `iterm`, `kitty`, `kitty-unicode`

By default `auto` will guess the best supported protocol for the current terminal (if listed in [Supported terminal emulators](./compatibility.md#supported-terminal-emulators)).

## -o, --order \<TYPE\>

Commit ordering algorithm.

_Possible values:_ `chrono`, `topo`

`chrono` will order commits by commit date if possible.

<img src="https://raw.githubusercontent.com/lusingander/serie/master/img/order-chrono.png" width=300>

`topo` will order commits on the same branch consecutively if possible.

<img src="https://raw.githubusercontent.com/lusingander/serie/master/img/order-topo.png" width=300>

## -g, --graph-width \<TYPE\>

The character width that a graph image unit cell occupies.

_Possible values:_ `auto`, `double`, `single`, `hidden`

If not specified or `auto` is specified, `double` will be used automatically if there is enough width to display it, `single` otherwise.

`hidden` will start without the graph column, leaving its width to the other columns. This is useful in a terminal that cannot display images at all, where the graph column would otherwise be reserved and stay blank. Unlike the other values, `hidden` never fails on a narrow terminal.

The graph can be shown and hidden again at any time with the `graph_toggle` keybinding (<kbd>T</kbd> by default), whichever value is used here.

<img src="https://raw.githubusercontent.com/lusingander/serie/master/img/graph-width-double.png" width=300>

<img src="https://raw.githubusercontent.com/lusingander/serie/master/img/graph-width-single.png" width=300>

</details>

## -s, --graph-style \<TYPE\>

How the commit graph is drawn.

_Possible values:_ `rounded`, `angular`, `ascii`, `unicode`

`rounded` and `angular` render the graph as images and need a supported terminal image protocol.
`ascii` and `unicode` render it as text instead, which works in any terminal — no protocol, no
image upload — and makes `--protocol` and `--graph-width` irrelevant.

`rounded` will use rounded edges for the graph lines.

<img src="https://raw.githubusercontent.com/lusingander/serie/master/img/graph-width-double.png" width=300>

`angular` will use angular edges for the graph lines.

<img src="https://raw.githubusercontent.com/lusingander/serie/master/img/style-angular.png" width=300>

`unicode` draws the graph with box-drawing characters, two terminal columns per lane, the same
spacing `git log --graph` uses:

```
●─╮  Merge branch 'topic'
● │  Commit on master
│ ●  Commit on topic
●─╯  Common ancestor
```

`ascii` draws the same graph with only `*`, `|`, `-` and `+`, for terminals or fonts without
box-drawing characters:

```
*-+  Merge branch 'topic'
* |  Commit on master
| *  Commit on topic
*-+  Common ancestor
```

Unlike `git log --graph`, a turn shares the row of the commit it belongs to rather than taking a
row of its own, so the graph has exactly one row per commit.

## -i, --initial-selection \<TYPE\>

The initial selection of commit when starting the application.

_Possible values:_ `latest`, `head`

`latest` will select the latest commit.

`head` will select the commit at HEAD.

## [REVSPEC]...

Revisions to render, passed to `git log` as-is. If not specified, all branches, remotes and tags are rendered.

When exactly two revisions are given, their common ancestor is marked in the commit list with a
`◆` in the marker column and a bold subject, and the `go_to_merge_base` keybinding
(<kbd>b</kbd> by default) jumps to it.

```
$ serie master topic
```

When two or more revisions are given, each one's tip is marked in the marker column with its
position in the revspec (`1`, `2`, `3`, ...), and the `go_to_next_tip` keybinding (<kbd>t</kbd> by
default) rotates through them, walking down the list and wrapping at the bottom. A commit that is
both a tip and the merge base shows the merge base marker, and is still reachable by rotation.

```
$ serie master topic feature
```

Ranges, exclusions and flags contribute no tip, but do not stop the other revisions from being
marked: `serie master topic --all` still marks `master` and `topic`.

The base is computed with `git merge-base`, so it is recomputed on refresh as the branches move.
Ranges (`master..topic`), exclusions (`^master`) and `git log` flags opt out, since none of them
names exactly two commits.
