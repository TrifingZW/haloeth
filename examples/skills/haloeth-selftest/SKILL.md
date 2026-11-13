---
name: haloeth-selftest
description: End-to-end self test for Haloeth skills, tools, approvals and reports.
when_to_use: the user says self test, 自检, 测试 skill, or asks to verify the agent works end to end
---

# Haloeth self test

You are running Haloeth's built-in self test. Execute every step in order and report
real observations, never assumptions.

## Steps

1. Read `checklist.md` from this skill's directory. The absolute directory path is on the
   `Directory:` line of this skill output; call `read_file` with that path joined to
   `checklist.md`.
2. Call `current_time` once. Record the returned UTC timestamp.
3. Ask the user for an output directory unless their message already names one. Never guess
   a directory and never write outside it.
4. Read `report-template.md` from this skill's directory, fill in every placeholder with the
   results from steps 1-2, then call `write_file` to create
   `<output directory>/haloeth-selftest-report.md`.
5. Call `final_answer` with a short table listing each step, the tool that ran, and the
   outcome (`OK`, `DENIED`, or the error text).

## Rules

- If a call is denied, record `DENIED` for that step and continue with the next one.
- If a file is missing, say which path failed instead of inventing content.
- Answer in the user's language; keep tool arguments and file contents in English.
