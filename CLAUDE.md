# saaga-social-flow

## Worktrees

The main checkout (`~/saaga-repo/saaga-social-flow`) stays on `master` and is
not worked in. Several sessions share it, and a branch switched there moves
under all of them. Every change is made in a worktree.

- **Where:** `.claude/worktrees/<branch>` inside this repo (gitignored), and
  nowhere else — not beside the repo in `~/saaga-repo`, not in
  `~/saaga-repo/saaga-worktrees`.
- **Name:** the folder is named after the branch, and the branch after the
  change (`youtube-write-button`, not `fix` or `wip`). One worktree per branch;
  a finished worktree is not reused for the next task.
- **Start** from the latest master:

  ```sh
  git fetch origin
  git worktree add .claude/worktrees/<branch> -b <branch> origin/master
  ```

- **Finish:** `cargo fmt -- --check`, `cargo clippy -- -D warnings` and
  `cargo test --release` pass in the worktree. Merge through a PR, or from
  inside the worktree onto the latest `origin/master`
  (`git checkout --detach origin/master && git merge --no-ff <branch>`, then
  `git push origin HEAD:master`) — never by checking the branch out in the main
  checkout. Then, in the same session:

  ```sh
  git worktree remove .claude/worktrees/<branch>
  git branch -d <branch>
  git pull --ff-only   # in the main checkout, when it is on master and clean
  ```

  Each worktree builds its own `target/`, about 10 GB, so a merged one is not
  left on disk.
- **Work to keep but not merge** goes on a branch, pushed if it matters, and
  its worktree is removed all the same.
- A worktree has no `.env` and no `renderer/node_modules`. Tests do not need
  them; to run the app from a worktree, copy `.env` from the main checkout and
  run `scripts/setup.sh`.
