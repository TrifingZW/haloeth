# Self test checklist

| # | Check | Expected |
| - | ----- | -------- |
| 1 | Skill loaded | This file was readable from the skill directory |
| 2 | Resource listing | `report-template.md` appears next to this file |
| 3 | Current time | `current_time` returned a UTC timestamp ending in `UTC` |
| 4 | Write approval | Writing the report either asked for approval or used a write grant |
| 5 | Final answer | A table was produced by the `final_answer` tool |

Record `OK` only when the observation matches the expected value.
