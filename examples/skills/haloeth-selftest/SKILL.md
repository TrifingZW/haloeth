---
name: haloeth-selftest
description: End-to-end self test for Haloeth skills, tools, approvals, search, files, image generation and image display.
when_to_use: the user says self test, 自检, 测试 skill, 测试所有功能, or asks to verify the agent works end to end
---

# Haloeth self test

You are running Haloeth's built-in self test. Execute the steps in order, observe what really
happens, and report every step as `OK`, `SKIPPED`, `DENIED` or `FAILED`. Never invent an
observation.

## Before you start

1. Ask the user for an output directory unless their message names one. Use it for the report
   and any generated files. Never guess a path and never write outside it.
2. The absolute directory of this skill is on the `Directory:` line of the skill output.

## Steps

1. **Skill resources** — call `list_dir` on the skill directory. Expect `checklist.md`,
   `report-template.md` and `sample.png`.
2. **Show an image** — call `show_image` with the absolute path
   `<skill directory>/sample.png`. The user should see the image inside the conversation.
3. **Current time** — call `current_time` once and record the UTC timestamp.
4. **Read a file** — call `read_file` on `<skill directory>/checklist.md` and record whether
   its first line is visible.
5. **Write a file** — call `write_file` to create `<output directory>/haloeth-selftest-report.md`
   with the report template filled in. Record whether the write was allowed, denied, or asked
   for approval.
6. **Shell** — call `shell` with `echo haloeth-selftest`. Record the output or the denial.
7. **Search** — if `web_search` is in the tool list, search for `Haloeth GPUI Rust agent` and
   record how many results came back plus the first title. If the tool is absent, mark
   `SKIPPED` (no search provider configured).
8. **Web fetch** — if step 7 returned a URL, call `fetch_url` on it and record whether a title
   or some text came back. Otherwise mark `SKIPPED`.
9. **Image generation** — if `generate_image` is in the tool list, ask the user for a file path
   inside the output directory, then generate a small image (for example "a small blue dot on a
   white background"). Record the saved path; then call `show_image` on it. If the tool is
   absent, mark `SKIPPED` (no image model configured). If it fails, record the provider's exact
   error text.
10. **Final answer** — call `final_answer` with a table of every step: step number, tool used,
    result (`OK` / `SKIPPED` / `DENIED` / `FAILED`), and one line of observation. Do not call
    any other tool in the same step.

## Manual checks for the user

These cannot be exercised by a tool, so ask the user to confirm them in 偏好 → 无障碍:

1. **Microphone** — click 测试麦克风, speak one sentence, stop; a transcript should appear next to the button.
2. **TTS** — click 试听播报; the configured voice should speak.
3. **Image viewer** — click the image shown in step 2; a separate window should open with rotate, mirror and zoom controls.

Record the user's answers in the final table under a "manual" note.

## Rules

- If a call is denied, record `DENIED` for that step and continue; do not retry it.
- If a tool is missing, mark `SKIPPED` instead of failing the whole test.
- If a file is missing, say which path failed instead of inventing content.
- Keep tool arguments and file contents in English; answer the user in their language.
- Mention the current approval mode if the user asked about approvals: whether a write or shell
  call was auto-approved, asked, or denied is the most useful signal.
