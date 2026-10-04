# Documentation style

The register, the emphasis rules, and the punctuation conventions every document in this repository
is written to, together with the terminology it uses. A reference manual describes a system; it does
not argue for one.

Most of what follows is subtractive. Where a rule and a habit disagree, the rule wins and the habit
is the defect.

## Scope

Every document in the repository is written to this guide except CLAUDE.md and PROMPT.md

## Register

Documentation is written in the third person, in the present tense, about the library.

- The subject of a sentence is the thing being described, not the reader and not the author. Write
  "an exhausted preference list raises `NoCandidate`", not "you will get an error back".
- Second person is correct in one place: the numbered steps of a procedure, where the imperative is
  the clearest form. "Drain the node." "Reload the topology." Everything around those steps returns
  to the third person.
- Do not address the reader's expectations, assumptions, or feelings. "That is the library working,
  not failing" and "worth knowing before you start" describe a conversation rather than a system.
- Do not write in the first person, singular or plural. The documentation has no narrator.

## Emphasis

Bold marks the first occurrence of a defined term in the document that defines it. It has no other
use. Bold applied to a clause for stress is the most common defect in this corpus, and its effect
is cumulative: where a fifth of the text is emphasised, emphasis carries no information.

- No bold for stress, contrast, warning, or surprise.
- No capitalised words for stress. Capitals are for acronyms, identifiers, enum constants, HTTP
  methods, environment variables, and other things that are genuinely spelled that way.
- The RFC 2119 keywords are spelled that way. `MUST`, `MUST NOT`, `SHOULD`, `SHOULD NOT`, and `MAY`
  are capitalised in the normative specification, where they carry their RFC 2119 meaning, and
  nowhere else. A document that is not normative says "refuses" rather than "`MUST` refuse".
- No italics for stress. Italics mark a term quoted as a term, and little else.

Where a fact is important, give it its own sentence, its own paragraph, or its own heading. Position
carries emphasis in a reference manual; typography does not.

## Headings

A heading is an index entry and a link target. It labels the material beneath it.

- Write a noun phrase. "Preference list construction", not "why the preference list is ordered by
  failure domain and not by hash".
- No commas, no conjunctions joining two clauses, no question forms, no verbs of judgement
  ("deliberately", "worth", "why").
- Eight words is the practical ceiling.
- Headings are link anchors. Renaming one is an interface change, and a rename that breaks a
  cross-reference fails the build in the commit that makes it.

## Justification

State what the library does. Explain a decision only where a reader who does not know it would draw
a wrong conclusion, and then explain it plainly, in its own sentence.

- Do not defend a design against an imagined objection. "That is deliberate rather than unfinished",
  "rather than an oversight", and "this is the design working" all answer a criticism nobody reading
  a manual has made.
- Do not certify a claim's provenance in passing. "Measured, not reasoned about", "read out of the
  source", and "and none of it is hedged" are assurances about the author's diligence. Where
  provenance genuinely matters, such as a benchmark's conditions, it is content: give it a sentence
  that says what was measured, on what, and when.
- Do not write about the document. A document does not explain why it exists, why it is separate
  from another document, how many times it has been corrected, or what it is not. Routing belongs in
  the directory's entry point.

A decision record under `docs/design/adr/` is the one place justification is the content rather than
a defect. Its Context, Consequences, and Alternatives sections say why a decision was taken and what
was rejected, because recording that is the document's purpose. Every other rule in this guide
applies to it unchanged: third person, no bold for stress, noun-phrase headings. The carve-out
covers what an ADR is allowed to discuss, not how it is allowed to sound.


## Punctuation and mechanics

- No em dashes and no spaced double hyphens. Use a comma, a semicolon, a colon, parentheses, or two
  sentences.
- Use a serial comma.
- British spelling in prose: `behaviour`, `serialise`, `normalise`, `initialise`. Technical terms
  keep the spelling of the thing they name: an identifier is quoted exactly as the source spells it,
  so a field named `normalizedWeight` stays `normalizedWeight`.
- Code formatting for anything a machine reads: identifiers, file paths, property keys, environment
  variables, requirement identifiers, literal values, wire field names.
- Wrap prose at 100 columns.
- Tables take a header row that names the columns. A table is for material with a repeating shape;
  prose with pipes in it is not a table.

