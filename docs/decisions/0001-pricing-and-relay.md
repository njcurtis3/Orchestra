# ADR 0001 — Pricing tiers and the hosted relay

**Date:** 2026-10-18
**Status:** Accepted 2026-10-18 (prices are starting points, not research)
**Scope:** `orchestra/` only. Lives here, not in `graph_agents/decisions/`, because it is about one product.

## The decision

1. **Orchestra is sold as a free tier plus a paid Pro tier.** Team and Lifetime are noted below
   but not decided.

   | Tier | Price | Includes |
   |---|---|---|
   | Free | $0 | Desktop app, unlimited sessions and inbox, 1 project, tasks with local land, the direct phone link |
   | Pro | about $10-12/month or about $99/year | Unlimited projects, hosted relay and real push (below), PR landing (D6), fleet runs (D5), more than one agent (D7) |
   | Lifetime | about $149-199, capped run, launch only | Pro as it stands at purchase, for the life of the major version |
   | Team | per seat, later | Not decided. Shared hosts, shared policy, an audit trail of approvals |

   The anchor is One (getone.one) at $12.99/month. No other price data was collected.

2. **The direct link stays free and keeps working.** The P1/P3a link (LAN, tailnet or WireGuard
   the user already runs; Noise IKpsk2 / KK; no public port) is never gated. A free user can
   always reach their own host by that route.

3. **A hosted relay is a Pro convenience, built after the direct link works end to end.** It
   lets the phone reach a host with no network setup, and carries push to a sleeping phone.
   - The relay forwards Noise-encrypted frames. It holds no host key, no client key and no
     plaintext, so it can delay or drop traffic but not read or forge it.
   - It sees only what routing needs: an opaque pairing id, connection timing and sizes. It
     does not see agent names, states, machine names or project names. (One's servers see
     agent names and states; ours will not.)
   - Orchestra still never runs agents on our servers. The relay is transport only.
   - Push payloads are encrypted by the host before they reach FCM.

4. **Licensing is checked locally.** A Pro licence is a signed key the app verifies against a
   public key compiled into the build. There is no phone-home on launch, so "sends nothing
   anywhere" stays true for Free. Only the relay, which a user opts into, is a network
   service we run. Licence issue and renewal go through a merchant of record (Paddle or Lemon
   Squeezy), not our own tax handling.

5. **Telemetry stays off.** Pricing does not change the "sends nothing anywhere" rule in
   ROADMAP "Considered, not doing". Conversion is measured from sales, not from the app.

## Context

- **The relay is the only recurring cost we would carry.** Everything else Orchestra does runs on
  the user's hardware, so a subscription needs a reason to exist. A relay and push sender are
  that reason, and they are what One's subscription pays for.
- **P1 settled "no relay in v1".** `protocol/README.md` chose a direct link first, with a relay
  left as an ops concern. This record keeps that order and says what the relay is when it comes.
- **Differentiators are not the relay.** Worktree tasks with a guarded land, the fleet graph,
  and Windows and Linux support are why someone picks Orchestra over One. Charging for the relay
  alone would not fund them, so the Pro line also covers D5, D6 and D7.
- **Free has to be useful.** The people who convert are the ones already running several agents
  who then want the phone and the fleet. A crippled free tier loses them before that.
- **Constraint from the owner's setup:** no Mac (decision 0003 in `graph_agents/`), so an iOS
  or signed macOS tier is not on offer until that path exists. Pro is sold for Windows and
  Android first.

## Not decided here

- Exact prices, the Lifetime cap, and whether a trial replaces or sits beside Free.
- Whether Team is worth building, and what it would hold.
- The refund and cancellation terms, the EULA and the privacy policy. These need writing, and
  the LLC (Telos Research Group is not yet formed) should exist before money is taken.
- Relay hosting, region and abuse limits. That is an ops-gate item with R1 to R3.
- Free-tier project and session caps. 1 project is a placeholder.

## What would reverse this

- **herdr Cloud or Anthropic ship an equivalent free relay and phone app.** Then a paid relay
  has no reason to exist and Pro would have to rest on D5/D6/D7 alone, or the product stays free.
- **Direct-link setup proves too hard** for the target user. Then the relay moves into Free for
  adoption and the paid line moves elsewhere.
- **Nobody pays in the first cohort.** If the first paid launch converts below the point where
  relay costs are covered, drop the hosted relay and sell a one-time licence instead.
- **A single-vendor dependency bites.** If Claude Code's terms or output format change so that
  Orchestra can no longer ship as a paid product on it, D7 moves ahead of any pricing work.
- **The owner decides Orchestra stays a personal tool.** Then none of this applies.

## Order of work this implies

Pricing is not a milestone. It sits behind: P3b, A1 and A2 (a phone that works over the direct
link), R1 and R2 (signed builds), the LLC, then licensing and payments, then the relay. Writing
this down does not move anything in ROADMAP.

## This is a decision record, not a dependency

Nothing in the build reads this file. Delete it and only the next reader's ability to skip the
argument goes with it.
