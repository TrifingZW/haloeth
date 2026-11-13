# Self test checklist

| # | Check | Expected | Status |
| - | ----- | -------- | ------ |
| 1 | Skill loaded | This file was readable from the skill directory | |
| 2 | Resource listing | `sample.png` and `report-template.md` are listed | |
| 3 | Image display | `show_image` shows `sample.png` in the conversation, clicking opens the viewer | |
| 4 | Current time | `current_time` returned a UTC timestamp ending in `UTC` | |
| 5 | File read | `read_file` returned the first line of this checklist | |
| 6 | File write | `write_file` created the report in the output directory (or asked / denied) | |
| 7 | Shell | `echo haloeth-selftest` returned `haloeth-selftest` (or asked / denied) | |
| 8 | Search | `web_search` returned at least one result (or SKIPPED if absent) | |
| 9 | Web fetch | `fetch_url` returned text for a result URL (or SKIPPED) | |
| 10 | Image generation | `generate_image` saved an image (or SKIPPED / FAILED with the provider error) | |
| 11 | Final answer | A table was produced by the `final_answer` tool | |

Record `OK` only when the observation matches the expected value. Use `DENIED` when the
approval policy refused a call, and `FAILED` when the tool returned an error.
