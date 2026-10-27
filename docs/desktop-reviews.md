# Reviews

Reviews in the desktop app is where you review pull requests on GitHub. It
lists every open pull request that wants you. Open one to read its diff with
full context, its threads and its checks. From there you can comment, approve,
merge, or check the branch out, and send the review to the agent that wrote
it.

Press <kbd>Cmd</kbd>+<kbd>Alt</kbd>+<kbd>R</kbd> to open it.

It talks to GitHub through your own [GitHub CLI](https://cli.github.com)
(`gh`), so sign-in, hosts and GitHub Enterprise work exactly as they do in a
terminal. If `gh` is missing or signed out, Reviews says so; run
`gh auth login` and press <kbd>R</kbd>. See the
[desktop overview](./clients.md#desktop) for the rest of the app, and the
[Changes panel](./desktop-changes.md) for uncommitted work.

## Open it

**Toggle Reviews** opens and closes the surface in a normal window; the
quick terminal has no room for it. Reach it with
<kbd>Cmd</kbd>+<kbd>Alt</kbd>+<kbd>R</kbd> (<kbd>Super</kbd> instead of
<kbd>Cmd</kbd> on Linux), the pull request button in the titlebar of a window
at least 900 points wide, **View > Toggle Reviews**, or the command palette.
Reviews takes the panes' place while it is open. The panes keep running, and
choosing a tab, splitting, or opening a terminal closes Reviews so you see
the result. Your unsent review is kept when you close it.

## The inbox

The left column lists open pull requests, grouped by repository, the most
recently active first. It holds three kinds:

| Kind | Which |
|---|---|
| Mine | Pull requests you opened, including those an agent pushed under your account. |
| Requested | Pull requests waiting for your review, or your team's. |
| Others | Every other open pull request in your own account and in organizations you administer. |

**All**, **Mine**, **Requested** and **Others** filter the list. Each row
shows the head commit's checks as a dot (green passing, amber running, red
failing), the number, author, last activity and size. Chips show what stands
in the way, such as **Draft**, **Conflicts**, **Approved**, **Changes**,
**Review** (yours is requested), and open threads. Each search lists at most
100 pull requests. The inbox rereads every 90 seconds while open.

## A pull request

The header shows the title, `repository#number`, author and branches, checks
and review decision. It also says where the pull request stands, such as
**Ready to merge**, **Blocked by required checks or reviews** or **Conflicts
with the base**, and names the agent working on the branch when one runs on
this Mac. The opened pull request rereads every 30 seconds. A new push
reloads its diff unless you have unfinished line comments; those keep the
original diff and commit until submitted or removed. A banner says when the
review target changed. A changed base must be reviewed again before submitting.

### Files

The file list shows every changed file with its line counts, open threads,
and **Viewed** mark. A **changed** label means you viewed the file and it
changed since. Click a file to scroll the diff to it.

The diff reads from a clone on this Mac when there is one. It looks in the
folders your panes are in, folders you opened in the app, and the usual places
such as `~/workspace/<owner>/<name>` or `~/src/<name>`. The local diff has every line of
context, so any fold opens. If the clone lacks the pull request's commits,
Reviews fetches them without moving any branch or writing `FETCH_HEAD`.
Without a clone, the diff is GitHub's, with three lines of context. The
toolbar says which one you are reading.

As in the Changes panel, changed words are emphasized and syntax is colored.
The layout button cycles **Automatic**, **Side by Side** and **Unified**;
Automatic splits once the diff is about 1100 points wide. **N** and **P**
jump between changes.

Moving to another pull request keeps the review you were writing: its kept
comments, unfinished comment field, summary and verdict come back when you
return to it. Switching waits for an in-flight submission or reply to finish.

**Viewed** in a file header, or the eye in the file list, marks the file
viewed on GitHub too and folds it. GitHub's own marks fold files when the pull
request opens.

### Threads and comments

GitHub's review threads sit under the lines they are about. A thread whose
lines changed since it was written sits under its file's header, marked
**Outdated**. Resolved threads fold to one line; **Show** opens them.
**Reply** posts at once, and **Resolve** and **Reopen** change the thread on
GitHub.

To comment, hover a line, or select lines from the gutter, and click the add
button. **Enter** keeps the comment for your review and **Shift+Enter** adds a
line. Kept comments show as **Pending** under their lines until you submit.

### The review

The bar under every tab holds the review's summary and verdict.
**Comment**, **Approve** and **Request Changes** set the verdict; GitHub takes
only comments on your own pull request. **Submit Review** posts the summary,
the verdict and every kept comment as one review, in one notification.

When an agent works on the pull request's branch, **Also paste into** sends
the same review to it after GitHub takes it: Reviews shows the agent's pane
and pastes the notes, numbered with file and line, along with any thread
still open on GitHub. Nothing is submitted to the agent: press **Enter**
there. **Agent Only** pastes the notes without posting to GitHub, and keeps
your review so a failed handoff cannot lose your notes.

An agent is found by its pane: a pane with an agent, whose folder is a
checkout of the repository on the pull request's branch.

### Conversation and checks

**Conversation** shows the description, comments and submitted reviews,
oldest first. **Checks** lists every check on the head commit, failures
first. **Log** on a failed Actions job shows the step that failed, through its
error, without timestamps or color codes. The arrow opens a check on GitHub.

## Merge

The header offers the merge methods the repository allows, preferring the one
you last used. Click **Merge** (named for the method) once to arm it and
again to merge. Escape disarms it, and so does a changed diff. Merge stays
disabled until the current diff has loaded. The merge is of the
head you reviewed: if someone pushed since, GitHub refuses and the diff
reloads. The repository's own setting decides whether the branch is deleted;
GitHub then retargets any pull request based on it. After merging, Reviews
fast-forwards the base branch in the clone here. It leaves a base with
uncommitted changes or commits of its own alone, and says so.

**Auto-merge** merges once required checks and reviews pass, where the
repository allows it. **Ready for Review** takes a draft out of draft.

## Check out

**Check Out** opens the branch in a linked worktree of the clone, with a
session of its own, through `phux worktree new`, so worktree hooks run. A
fork's branch checks out as `pr-<number>`. When a worktree is already on the
branch the button reads **Open Worktree** and opens that one instead.

## Keys

While no text field has the keyboard:

| Keys | Action |
|---|---|
| <kbd>J</kbd> / <kbd>K</kbd>, <kbd>↓</kbd> / <kbd>↑</kbd> | Next / previous pull request |
| <kbd>N</kbd> / <kbd>P</kbd> | Next / previous change |
| <kbd>1</kbd> <kbd>2</kbd> <kbd>3</kbd> | Files, Conversation, Checks |
| <kbd>C</kbd> | Write the review's summary |
| <kbd>R</kbd> | Refresh |
| <kbd>O</kbd> | Open on GitHub |
| <kbd>Esc</kbd> | Leave the summary, drop the comment being written, disarm Merge, or close Reviews |

## Limits

- Reviews reads GitHub through `gh` on this Mac, and only clones on this Mac;
  a window on another machine shows GitHub's diff and cannot check out.
- Threads show their first 100 comments, and a pull request its first 100
  threads, comments and reviews, and 3000 files.
- GitHub-hosted repositories only; other forges are not supported.
