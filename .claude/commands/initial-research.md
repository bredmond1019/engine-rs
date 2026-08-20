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

3. Act as a senior engineer conducting a deep reconnaissance mission. Investigate the topic
   thoroughly by reading code, documentation, and existing architectural decisions — but reading
   is not the only mode. **Where the subject can be run, run it once; that outranks reading about
   it.** A command, a script, a test, a server endpoint observed actually executing settles a
   question that reading its source can only suggest. Read first to know what to run, then run it.
4. **Critical Mandate**: You must gather ALL important details during your investigation. Do not just summarize. You MUST collect:
   - File paths
   - Class/struct names, function signatures
   - Important snippets of relevant code
   - Any architectural nuances, constraints, or context that clarifies *why* things are the way they are.
   - Pointers on where to look to review/investigate further.
   This content must be EXTREMELY easy for the next agent or the user to dig into and know exactly what was discovered and how you reached your conclusions.

   **Mark every substantive finding's standing.** A research report is read weeks later, by
   someone with none of this session's context, and read as *fact* unless it says otherwise.
   Prefix or tag each substantive finding (same vocabulary as `/capture`):

   | Tag | Means |
   |---|---|
   | **VERIFIED** | Read in source or observed running, this session. Name the file and symbol |
   | **ASSUMED** | Believed, not checked. Say what would check it |
   | **SAID** | The user or another agent stated it; not independently confirmed |

   An untagged finding is indistinguishable from a verified one, and it will be planned on as if
   it were. Tagging costs a word and is the difference between a research report and a confident
   guess.

### Step 3 — Format the report

5. Structure your findings clearly with headers for:
   - **Goal/Topic**: What was investigated.
   - **Key Findings**: The core discoveries, each tagged VERIFIED / ASSUMED / SAID.
   - **Technical Details**: The file paths, class/structs, functions (name the symbol, not just a
     line number — line numbers move between authoring and reading), and code snippets.
   - **Conclusions & Next Steps**: How we got to this conclusion and where to investigate further.
   - **Provenance**: Today's date, and for each repo the report makes claims about, its
     `git rev-parse --short HEAD`. A reader who knows the SHA can tell in one command whether the
     report still describes the system that exists.

### Step 4 — Capture (if requested)

6. If the `--capture` flag was provided in `$ARGUMENTS`, you must automatically run the `/capture` command, passing the title of your research and all the comprehensive details you gathered so they populate the body of the generated `notes.md` file. 

### Step 5 — Report

7. Present the structured report to the user. If `--capture` was used, confirm the path to the newly created notes file. If `--capture` was not used, remind the user that they can easily run `/capture` to save this research if they decide to keep it.

## Context / Files to Read

- None — the agent should dynamically explore based on `$ARGUMENTS`.
