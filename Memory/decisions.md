# Decisions

- Familiar owns all reasoning and planning; it never executes an Effect
  itself. Every Effect is Servitor work requested through the feed.
- Retrieval/Effect boundary: operator-enumerated read-only local retrieval
  is allowed only in authenticated local interactions; network-originated
  turns receive no tools.
- Familiar publishes through the local Egregore node and holds no network
  keypair of its own.
- Familiar is a decision client of RFC 0003; typed assignment commands are
  the contract, bare task_assign is compatibility only.
- Single main branch; CI green before pushing.
- This repo carries its own Memory v2 pair; cross-component decisions live
  in the Thallus umbrella. Machine-local state stays under ignored Work/.
