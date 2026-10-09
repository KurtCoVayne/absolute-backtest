You are writing a trading strategy in "abt", a Datalog-like strategy language you have never seen before. Your workspace is the directory SANDBOX. cd into it first; all paths below are relative to it.

Read TASK.md: it describes the strategy, the data and the run conventions.
What you have:
- docs/README.md (the syntax in one page, checker codes) and docs/semantic-model.md (the semantics): the language description.
- env/*.dsl (the data environments: the relations the data provides) and lib/*.dsl (standard libraries you may `uses`).
- bin/abt, the toolchain. `bin/abt` with no arguments prints the usage. `bin/abt check env lib YOURFILE.dsl` checks a strategy (read its stdout: it lists errors and warnings; the exit code is not reliable). `bin/abt run --strategy NAME ... env lib YOURFILE.dsl` runs it.

Rules (strict, this is an experiment on how usable the language is):
- Work ONLY inside the workspace. Do not read, list or search any other file or directory on this machine (no `find /`, no reading of other projects, no source code of abt), except the data directory named in TASK.md, which you pass to `bin/abt` and should not need to read. Do not use the web.
- Save EVERY version of your strategy before you check or run it, as attempts/1.dsl, 2.dsl, 3.dsl, ... (copy the file; never overwrite an earlier attempt). Each attempt is a complete file.
- Budget: at most about 30 `bin/abt` invocations in total. Stop when your strategy checks clean, runs, and you believe it implements the specification faithfully.
- When done, write:
  1. final.dsl (your final strategy),
  2. run.sh (the exact `bin/abt run ...` command you would use for the full backtest per the run conventions, run from SANDBOX),
  3. NOTES.md: what was hard, what the docs lacked or got wrong, which constructs you guessed at or that did not exist, which rules of the spec you are unsure you implemented faithfully, and your run's headline results (decisions, fills, the metrics line).
Finally reply with a short summary (under 200 words): attempts made, whether final.dsl checks clean, how many decisions the final run made, and the biggest difficulties.
