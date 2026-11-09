# Candidate first contributions

These are scoped issue drafts. Check the current code and open issues before starting;
the descriptions do not imply that these issues have been published or assigned.

## Add a deterministic example template

Propose a small template using existing nodes, without a paid provider requirement.
Include declared variables, module requirements, an example invocation and a test
that applies the template and verifies its observable output. No new core behavior.

## Improve one contributor-guide example

Follow the English module guide on a fresh checkout and report one missing step or
incorrect command. Update that section and verify every command you changed. Avoid
translating the entire design archive in the same PR.

## Document a module's failure path

Choose an existing node, reproduce a missing-input or failed-operation case using a
temporary profile, and add a concise example explaining the emitted error and recovery.
If the behavior is broken, first submit a minimal reproduction and focused regression.

## Add coverage for an existing template variable

Choose a variable whose valid or invalid boundary lacks coverage. Add a deterministic
integration test for that input, with an assertion on the diagnostic or result. Do not
mirror the template implementation or require a real provider account.
