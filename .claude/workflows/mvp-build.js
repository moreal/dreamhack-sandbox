export const meta = {
  name: 'wargame-mvp-build',
  description: 'Implement PLAN.md tasks 1-9 with Sonnet implementers, reviewer gates, worktree merges and per-stage commits',
  whenToUse: 'Rebuilding the wargame-vm MVP from DESIGN.md + PLAN.md',
  phases: [
    { title: 'Stage A', detail: 'Task 1: core domain, ports, errors, config' },
    { title: 'Stage B', detail: 'Tasks 2, 3, 4 in parallel worktrees' },
    { title: 'Stage C', detail: 'Tasks 5, 6 in parallel worktrees' },
    { title: 'Stage D', detail: 'Task 7: CLI + composition root' },
    { title: 'Stage E', detail: 'Tasks 8, 9 in parallel worktrees' },
  ],
}

const REPO = args.repo
const BRANCH = args.branch
const CHECK = "nix develop --command bash -c 'cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace'"

const TASKS = {
  1: { title: 'Core domain, ports, errors, config', crates: ['wargame-core'] },
  2: { title: 'Core security helpers, infra adapters, test fakes', crates: ['wargame-core'] },
  3: { title: 'Dreamhack provider crate', crates: ['wargame-dreamhack'] },
  4: { title: 'Tart provider crate', crates: ['wargame-tart'] },
  5: { title: 'Application services', crates: ['wargame-core'] },
  6: { title: 'Profile image builder, profiles, guest bootstrap', crates: ['wargame-tart', 'profiles/', 'guest/'] },
  7: { title: 'CLI binary and composition root', crates: ['wargame-cli'] },
  8: { title: 'TUI', crates: ['wargame-cli'] },
  9: { title: 'README, e2e smoke, nix wrapper, doctor polish', crates: ['README.md', 'scripts/', 'flake.nix'] },
}

const IMPL_SCHEMA = {
  type: 'object',
  properties: {
    summary: { type: 'string' },
    files: { type: 'array', items: { type: 'string' } },
    deviations: { type: 'string', description: 'Any deviation from PLAN.md/DESIGN.md, crates added, or things left undone. "none" if none.' },
    checks_pass: { type: 'boolean', description: 'true only if fmt, clippy -D warnings and cargo test --workspace all passed at the end' },
    worktree: { type: 'string', description: 'absolute path of the directory you worked in' },
    branch: { type: 'string', description: 'git branch name in that directory' },
  },
  required: ['summary', 'files', 'deviations', 'checks_pass', 'worktree', 'branch'],
}

const REVIEW_SCHEMA = {
  type: 'object',
  properties: {
    approved: { type: 'boolean' },
    issues: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
          file: { type: 'string' },
          description: { type: 'string' },
        },
        required: ['severity', 'file', 'description'],
      },
    },
    summary: { type: 'string' },
  },
  required: ['approved', 'issues', 'summary'],
}

const MERGE_SCHEMA = {
  type: 'object',
  properties: {
    merged: { type: 'array', items: { type: 'string' } },
    checks_pass: { type: 'boolean' },
    commit: { type: 'string' },
    notes: { type: 'string' },
  },
  required: ['merged', 'checks_pass', 'commit', 'notes'],
}

const TASK_NOTES = {
  3: 'Fixtures already exist in crates/wargame-dreamhack/fixtures/ (search.json, detail.json, search_page.json, login_401.json, download_401.json); use them, do not re-download.',
  6: 'Put the bootstrap.sh shell test harness at crates/wargame-tart/tests/bootstrap_sh.rs (the CLI crate does not exist yet); it should skip gracefully when bash/unzip/zip are unavailable. Profiles YAML must parse with wargame_core::domain::profile::Profile::from_yaml.',
  7: 'The TUI is Task 8; for now `wargame` with no subcommand prints "TUI not built yet" and exits 2. Embed profiles/*.yaml and guest/bootstrap.sh with include_str! relative to the crate (use concat!(env!("CARGO_MANIFEST_DIR"), "/../../profiles/...")).',
  8: 'Wire `wargame` with no subcommand to tui::run(ctx). Smoke-run the TUI once with WARGAME_HOME=$(mktemp -d) WARGAME_PROVIDER=fixture WARGAME_VM=fake under `script -q /dev/null` if a pty is available; otherwise rely on reducer tests.',
  9: 'Keep flake.nix changes minimal: the wrapper and checks already exist; add checks only if missing. README must document `nix run`, `nix build`, `nix develop`, first-run steps (doctor, auth login, profile build, launch), what "isolated" networking means (`tart run --net-host`), and the security model summary. Verify `nix flake check` passes (it is slow; run it once at the end).',
}

function implPrompt(n, dirText, cdText, extra) {
  const t = TASKS[n]
  return `You are the implementer for **Task ${n}: ${t.title}** of the wargame-vm MVP.

Working directory: ${dirText}. Work ONLY inside it.

Read, in this order, before writing any code:
1. PLAN.md — the "Global Constraints" section and the full "Task ${n}" section (your spec; other tasks' sections tell you the names your neighbours use).
2. DESIGN.md — the sections the task references (§3 core, §4 dreamhack, §5 tart, §6 profiles, §7 cli, §3.6 security).
3. The existing code under crates/ (earlier tasks are already merged; reuse their exact names/signatures — do not rename or restructure them).

Rules:
- Test-driven: write the acceptance tests listed in the task's Step 1 first, watch them fail, then implement.
- Run all cargo commands through the Nix devShell from the working directory, e.g. \`${cdText}nix develop --command bash -c 'cargo test -p wargame-core'\`. Never install toolchains yourself.
- Do NOT add crates unless truly necessary; every dependency you need is already declared in Cargo.toml. If you must add one, add it to [workspace.dependencies] and regenerate Cargo.lock inside \`nix develop\`, and explain in "deviations".
- Follow the global constraints: argv arrays only (no shell strings), secrets never in argv/logs/metadata, no host-side extraction, no cross-dependency between wargame-dreamhack and wargame-tart.
- Keep files focused; match the file layout in PLAN.md's file map.
- Before finishing, run exactly: \`${cdText}${CHECK}\` and make it fully green (fmt, clippy -D warnings, all tests). Fix anything it reports.
- Do not touch files outside your task's scope except lib.rs/mod.rs declarations you need.
${TASK_NOTES[n] ? '- Task note: ' + TASK_NOTES[n] + '\n' : ''}${extra}
When finished, return the structured report (summary, files, deviations, checks_pass, worktree = absolute path you worked in (\`pwd\`), branch = \`git rev-parse --abbrev-ref HEAD\` there).`
}

function reviewPrompt(n, dir, report) {
  const t = TASKS[n]
  return `You are the reviewer for **Task ${n}: ${t.title}** of the wargame-vm MVP.

Working directory: ${dir}. Do NOT modify any file. Read ${dir}/PLAN.md (Global Constraints + Task ${n}) and the relevant DESIGN.md sections, then inspect the implementation.

Implementer report: ${JSON.stringify(report)}

Verify, with evidence (run the commands, read the code):
1. \`cd ${dir} && ${CHECK}\` is green. If it is not, that is a blocker.
2. Every acceptance test named in Task ${n} Step 1 exists and passes (grep for them).
3. The public names/signatures match the task's "Interfaces" block exactly (later tasks depend on them).
4. Global constraints: no shell-string subprocesses, no secrets in argv/logs/metadata, no host-side extraction/preview/execution of artifacts, no forbidden crate dependency, no new crates unless justified.
5. Code quality: no dead code, no TODO/unimplemented!/todo!, errors mapped to WargameError with hints, file layout per the file map.
6. Git hygiene: \`git status\` in ${dir} shows no stray files (target/, result are ignored).

Be strict on blockers (broken build/tests, spec/interface mismatch, security constraint) and majors (missing acceptance test, wrong behaviour). Minor style nits do not block approval. Return approved=true only if there are no blocker or major issues.`
}

function fixPrompt(n, dir, review) {
  return `You are fixing review findings for **Task ${n}: ${TASKS[n].title}** in ${dir}.

Review verdict: ${JSON.stringify(review)}

Read PLAN.md (Global Constraints + Task ${n}) and DESIGN.md as needed, fix every blocker and major issue (minors too if cheap), keep names/signatures from PLAN.md, then run \`cd ${dir} && ${CHECK}\` until green. Do not add crates. Return the structured report.`
}

async function runTask(n, { isolated, stage }) {
  const label = `task${n}`
  // Implement (isolated tasks get their own worktree from the current HEAD)
  const implOpts = { label: `${label}:impl`, phase: stage, model: 'sonnet', effort: 'high', schema: IMPL_SCHEMA }
  if (isolated) implOpts.isolation = 'worktree'
  const isoNote = isolated
    ? `\nYou are in an isolated git worktree. Other tasks run concurrently in their own worktrees; ignore them. When everything is green, commit your work IN THIS WORKTREE with \`git add -A && git commit -m "feat(task${n}): ${TASKS[n].title}"\` (add \`Co-Authored-By: Claude Sonnet <noreply@anthropic.com>\` as a trailer). Report the worktree path and branch name precisely; a merge step will merge your branch.`
    : `\nDo NOT commit; the orchestrator commits after review.`
  const dirText = isolated ? 'your current working directory, an isolated git worktree of the project (run `pwd` once and use that absolute path)' : REPO
  const cdText = isolated ? '' : `cd ${REPO} && `
  let report = await agent(implPrompt(n, dirText, cdText, isoNote), implOpts)
  if (!report) throw new Error(`task ${n}: implementer produced no report`)
  const dir = report.worktree || REPO
  log(`Task ${n} implemented in ${dir} (checks_pass=${report.checks_pass})`)

  let review = null
  for (let round = 0; round < 3; round++) {
    review = await agent(reviewPrompt(n, dir, report), { label: `${label}:review${round}`, phase: stage, effort: 'medium', schema: REVIEW_SCHEMA })
    if (!review) throw new Error(`task ${n}: reviewer produced no verdict`)
    log(`Task ${n} review round ${round}: approved=${review.approved}, issues=${review.issues.length}`)
    if (review.approved) break
    const fixNote = isolated ? `\nAfter fixing, commit again in ${dir} (git add -A && git commit -m "fix(task${n}): address review round ${round}").` : ''
    report = await agent(fixPrompt(n, dir, review) + fixNote, { label: `${label}:fix${round}`, phase: stage, model: 'sonnet', effort: 'high', schema: IMPL_SCHEMA })
    if (!report) throw new Error(`task ${n}: fixer produced no report`)
  }
  return { task: n, dir, branch: report.branch, approved: review.approved, review, report }
}

async function commitStage(stage, results) {
  const isolated = results.filter(r => r.dir !== REPO)
  const mergeList = isolated.map(r => `- task ${r.task}: worktree ${r.dir}, branch ${r.branch}`).join('\n')
  const prompt = isolated.length
    ? `You are the integrator for ${stage} of the wargame-vm MVP in ${REPO} (branch ${BRANCH}).

Merge these task branches into ${BRANCH}, in this order:
${mergeList}

Steps:
1. \`cd ${REPO} && git status\` must be clean and on ${BRANCH}; if not, stop and report.
2. For each branch: \`git merge --no-ff <branch> -m "merge: task N ..."\`. Resolve conflicts (usually crates/wargame-core/src/lib.rs module declarations or Cargo.lock; for Cargo.lock keep both sets of packages and regenerate with \`nix develop --command cargo generate-lockfile\` only if needed) by keeping BOTH sides' intent, then \`git add\` and \`git commit\`.
3. Run \`cd ${REPO} && ${CHECK}\`. If it fails because of integration issues (e.g. a name mismatch between crates), fix them minimally, re-run until green, and commit "fix(${stage}): integration".
4. Remove the merged worktrees: \`git worktree remove --force <path>\` for each, then \`git worktree prune\` and \`git branch -d <branch>\` (use -D if needed).
5. Return: merged branches, checks_pass, final commit hash (git rev-parse --short HEAD), notes.`
    : `You are the committer for ${stage} of the wargame-vm MVP in ${REPO} (branch ${BRANCH}).

Task results: ${JSON.stringify(results.map(r => ({ task: r.task, approved: r.approved, summary: r.report.summary })))}

Steps:
1. \`cd ${REPO} && ${CHECK}\` must be green (fix minimal issues if fmt changed files).
2. \`git add -A && git commit -m "feat(task${results.map(r => r.task).join(',')}): ${results.map(r => TASKS[r.task].title).join('; ')}"\` with trailer \`Co-Authored-By: Claude Sonnet <noreply@anthropic.com>\`.
3. Return merged=[], checks_pass, commit hash, notes.`
  const res = await agent(prompt, { label: `${stage}:integrate`, phase: stage, model: 'sonnet', effort: 'medium', schema: MERGE_SCHEMA })
  if (!res) throw new Error(`${stage}: integrator produced no report`)
  log(`${stage} integrated at ${res.commit} (checks_pass=${res.checks_pass})`)
  return res
}

const STAGES = [
  { name: 'Stage A', tasks: [1], isolated: false },
  { name: 'Stage B', tasks: [2, 3, 4], isolated: true },
  { name: 'Stage C', tasks: [5, 6], isolated: true },
  { name: 'Stage D', tasks: [7], isolated: false },
  { name: 'Stage E', tasks: [8, 9], isolated: true },
]

const summary = []
for (const s of STAGES) {
  phase(s.name)
  log(`${s.name}: tasks ${s.tasks.join(', ')}${s.isolated ? ' (parallel worktrees)' : ''}`)
  const results = (await parallel(s.tasks.map(n => () => runTask(n, { isolated: s.isolated, stage: s.name })))).filter(Boolean)
  if (results.length !== s.tasks.length) {
    summary.push({ stage: s.name, error: `only ${results.length}/${s.tasks.length} tasks produced results` })
    break
  }
  const integ = await commitStage(s.name, results)
  summary.push({ stage: s.name, tasks: results.map(r => ({ task: r.task, approved: r.approved, issues: r.review.issues })), commit: integ.commit, checks_pass: integ.checks_pass })
  if (!integ.checks_pass) { summary.push({ stage: s.name, error: 'integration checks failed; stopping' }); break }
}
return summary
