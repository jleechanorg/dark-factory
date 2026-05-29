# Spec: Spellcasting Pattern Evidence — PR #7142

## Goal

Prove that the three canonical spellcasting archetypes work end-to-end after
the ZFC refactor in PR #7142 (branch `feat-lu-paladin-spell-button-fix-2`):

1. **Prepared caster with dual storage (Wizard)** — character creation generates
   both `spells_known` (spellbook) and `spells_prepared`; level-up modal shows
   `level_up_change_prepared_spells` choice.

2. **Prepared caster without cantrips (Paladin)** — character creation generates
   `spells_prepared`; level-up modal shows spell selection once spell slots unlock.

3. **Known-spell caster (Sorcerer)** — character creation generates `spells_known`;
   level-up modal shows spell selection choice.

## Acceptance Criteria

Each class must complete an organic level-up campaign with:
- Character created successfully with correct spell fields populated
- Real LLM play-through using 34% XP pacing GOD MODE instruction
- Level-up detected and modal presented with appropriate spell choices
- `finish_level_up_return_to_game` step completes cleanly

## Test Command

```bash
TESTING_AUTH_BYPASS=true ALLOW_TEST_AUTH_BYPASS=true \
  PYTHONPATH="$(pwd):$(pwd)/mvp_site" \
  python3 testing_mcp/core/test_level_up_organic.py --class-name <wizard|paladin|sorcerer>
```

## Evidence Location

`/tmp/worldarchitectai/feat-lu-paladin-spell-button-fix-2/<test_name>/latest/`

Each run produces:
- `run.json` — scenario results (PASS/FAIL)
- `llm_request_responses.jsonl` — real Gemini API calls
- `http_request_responses.jsonl` — streaming endpoint captures
- `streaming_evidence.json` — streaming chunk verification

## ZFC Contract

Backend does NOT branch on class name. The LLM decides which spell fields to
populate based on canonical D&D rules provided in the prompt. `_spells_missing_for_class()`
only checks for any spell content — not class-specific field requirements.
