# Triage Labels

The skills speak in terms of five canonical triage roles. This file maps those roles to the actual label strings used in this repo's issue tracker. **This repo keeps the defaults.**

| Label in mattpocock/skills | Label in our tracker | Meaning                                  |
| -------------------------- | -------------------- | ---------------------------------------- |
| `needs-triage`             | `needs-triage`       | Maintainer needs to evaluate this issue  |
| `needs-info`               | `needs-info`         | Waiting on reporter for more information |
| `ready-for-agent`          | `ready-for-agent`    | Fully specified, ready for an AFK agent  |
| `ready-for-human`          | `ready-for-human`    | Requires human implementation            |
| `wontfix`                  | `wontfix`            | Will not be actioned                     |

When a skill mentions a role (e.g. "apply the AFK-ready triage label"), use the corresponding label string from this table.

## Labels this repo adds beyond the five roles

`/to-tickets` publishes tickets with `ready-for-agent` by construction — they are agent-grabbable, so **do not triage them**. The extra vocabulary this repo needs:

| Label | Used by | Meaning |
| ----- | ------- | ------- |
| `spec` | `/to-spec` | The synthesised spec for a feature; parents its tickets |
| `wayfinder:map` | `/wayfinder` | The map issue |
| `wayfinder:research` / `wayfinder:prototype` / `wayfinder:grilling` / `wayfinder:task` | `/wayfinder` | Child-ticket kinds |

Create labels with `gh label create "<name>" --description "<desc>"` before first use.
