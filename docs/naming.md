# Why "coop"?

The tool was originally called `claude-harness`, then briefly `moat`. Both were replaced.

`claude-harness` was functional but forgettable, and the hyphen made it awkward as a CLI command. `moat` had good practical qualities but the wrong metaphor — a moat keeps attackers *out* of a castle, while this tool keeps an agent *in* a VM. The containment direction is inverted.

## Selection criteria

- **Single word** — easy to type, easy to say
- **Not too exotic** — everyone should know the word
- **Evocative** — should suggest containment (keeping something in, not keeping things out)
- **Searchable** — uncommon enough as a tool name to find online
- **No ambiguity** — clear meaning in context

## Round 1: moat era

| Name | Pros | Cons |
|------|------|------|
| **coop** | Short, means enclosure | Ambiguous pronunciation: "co-op" vs "coop" |
| **keep** | Castle's inner fortress, 4 chars | Corny, extremely common word, unsearchable |
| **moat** | Uncommon as tool name, 4 chars, good typing | Metaphor is inverted — moats keep things out |
| **hutch** | Small enclosure, distinctive | 5 chars, `tch` is cramped to type |
| **den** | Private isolated space | Too generic, many tools use it |
| **ward** | To guard/protect | Too generic |
| **bailey** | Castle courtyard | Obscure, 6 chars |
| **fosse** | French/archaic for moat | Nobody knows it |
| **stockade** | Enclosure | Too long, aggressive connotation |

`moat` won this round, but the metaphor problem nagged.

## Round 2: containment-focused

Revisited with a stricter requirement: the name must convey keeping something *in*, not keeping things *out*.

| Name | Chars | Metaphor | Typing | Searchable | Cons |
|------|-------|----------|--------|------------|------|
| **pen** | 3 | Animal pen — keeps livestock in | p(R)-e(L)-n(R), perfect alternation | Weak — 3+ existing CLI tools named "pen" | Writing instrument ambiguity |
| **coop** | 4 | Chicken coop — enclosed structure | c(L)-o(R)-o(R)-p(R) | Very good — essentially vacant | "co-op" misread on first encounter |
| **brig** | 4 | Ship's jail — confinement | b(L)-r(R)-i(R)-g(L), nice alternation | Conflicts with Brigade's "brig" CLI | Punitive connotation |
| **silo** | 4 | Isolated container, "siloed" is tech jargon | s(L)-i(R)-l(R)-o(R), mostly right hand | Occupied — silo-rs/silo exists in Rust | Metaphor is more "isolation" than "containment" |
| **ark** | 3 | Self-contained vessel | a(L)-r(R)-k(R) | Decent | Grand/biblical, more "survival" than "enclosure" |
| **crate** | 5 | Shipping/animal container | Mixed hands | Good | Conflicts with Rust's `crate` keyword |

### Search landscape (March 2026)

Searched for each candidate + "CLI tool", "VM isolation", and "sandbox" to assess conflicts:

- **moat**: 4+ existing tools (Dusk Network SDK, password generator, Kubernetes security tool, moat-cli). Plus "moat" is an AI strategy buzzword ("where the moat lives in AI coding tools"). Crowded.
- **pen**: 3+ existing CLI tools (diary, multifunctional CLI, CodePen helper). Crowded.
- **brig**: Direct conflict with Brigade's CLI, which does developer-facing container/Kubernetes work. Same domain.
- **silo**: Occupied in the Rust ecosystem (silo-rs/silo, SiloWorker CLI, CGI-FR/SILO).
- **coop**: Essentially vacant. Only hit is `coop_cli`, a tiny interface for the UK Co-op grocery chain. No developer tools, no VM/container/isolation tools.

## Why coop won

A chicken coop is a small, purpose-built enclosure. The animal lives inside and operates freely, but it doesn't get out. The farmer reaches in as needed. That's exactly what this tool does — the VM is the coop, Claude runs freely inside, and you reach in through SSH and file sync.

The "co-op" pronunciation concern was the original blocker, but it dissolves in practice:

1. **In a terminal**, you read it — you don't say it. `coop start`, `coop ssh`, `coop destroy` are unambiguous.
2. **In speech**, context resolves it — "coop by Trail of Bits" or "the coop that isolates your coding agent" immediately triggers the enclosure meaning.
3. **In search**, "coop" + any qualifying term ("coop vm", "coop trailofbits", "coop agent isolation") is uncontested.

The word is:
- Universally known (everyone can picture a chicken coop)
- Essentially unoccupied as a tool name
- Four letters, fast to type
- Perfect containment metaphor — keeps things in, not out
- Visually distinctive and memorable
