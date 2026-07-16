# Initial Research — Conduct reconnaissance on a topic and report back

Conducts deep research into the codebase or external resources based on a detailed description. Reports back with highly structured, comprehensive context. Optionally captures this research directly into a pre-plan notes file if the `--capture` flag is provided.

## Variables

$ARGUMENTS — a detailed description of the research topic, optionally including the `--capture` flag.
             Example: "--capture How does our current authentication flow handle token refresh?"
             Example: "Investigate the memory leak in the data processing pipeline"

## Execution Model

Spawn a subagent (Agent tool) to execute all steps below. Pass the resolved `$ARGUMENTS` and this whole Instructions section in the subagent prompt. Return the subagent's result to the user.

## Instructions

### Step 1 — Parse arguments

1. From `$ARGUMENTS`, determine the core research goal.
2. Check if the `--capture` flag is present. 

### Step 2 — Conduct the research

3. Act as a senior engineer conducting a deep reconnaissance mission. Investigate the topic thoroughly by reading code, documentation, and existing architectural decisions.
4. **Critical Mandate**: You must gather ALL important details during your investigation. Do not just summarize. You MUST collect:
   - File paths
   - Class/struct names, function signatures
   - Important snippets of relevant code
   - Any architectural nuances, constraints, or context that clarifies *why* things are the way they are.
   - Pointers on where to look to review/investigate further.
   This content must be EXTREMELY easy for the next agent or the user to dig into and know exactly what was discovered and how you reached your conclusions.

### Step 3 — Format the report

5. Structure your findings clearly with headers for:
   - **Goal/Topic**: What was investigated.
   - **Key Findings**: The core discoveries.
   - **Technical Details**: The file paths, class/structs, functions, and code snippets.
   - **Conclusions & Next Steps**: How we got to this conclusion and where to investigate further.

### Step 4 — Capture (if requested)

6. If the `--capture` flag was provided in `$ARGUMENTS`, you must automatically run the `/capture` command, passing the title of your research and all the comprehensive details you gathered so they populate the body of the generated `notes.md` file. 

### Step 5 — Report

7. Present the structured report to the user. If `--capture` was used, confirm the path to the newly created notes file. If `--capture` was not used, remind the user that they can easily run `/capture` to save this research if they decide to keep it.

## Context / Files to Read

- None — the agent should dynamically explore based on `$ARGUMENTS`.
