# Session Checkpoint

**LAST UPDATED:** 2026-02-25

---

## Current Focus

**GOAL:** Propagate revised BSL licensing terms to all repos and create website change document
**PHASE:** implementation
**SUCCESS CRITERIA:** All LICENSE/README/CONTRIBUTING/metainfo files updated in both dev and public repos; website change document created in ~/lamco-admin

---

## Decisions Made (Do Not Revisit)

| Decision | Rationale | Do NOT |
|----------|-----------|--------|
| BSL Additional Use Grant: "single server instance + non-profits" replaces "≤3 employees AND <$1M revenue" | Simpler, enforceable, closes multi-server loophole | Revert to employee/revenue test |
| All paid tiers become subscriptions (no perpetual) | Recurring revenue, industry standard, prevents Change Date exploitation | Offer perpetual licenses |
| Service Provider tier eliminated | Unlimited for flat fee is indefensible at scale | Recreate an unlimited tier |
| Change Date stays 2028-12-31 for v1.x | Already established; per-version rolling starts at v2.0 | Change the Change Date for v1.4.0 |
| Competitive Use definition unchanged | Well-written, appropriate, already published | Modify competitive use clauses |
| CHANGELOG.md old entries left as-is | Historical record of terms at time of release | Edit past changelog entries |
| No AI attribution in commits | User explicitly requires this | Add Co-Authored-By lines |
| D-Bus is the correct IPC (not Varlink) | Desktop apps universally use D-Bus | Suggest Varlink or alternatives |
| QEMU D-Bus support goes in lamco-rdp-server, not separate binary | Reuses entire pipeline; ~1000-1800 LOC vs ~2500 from scratch | Build standalone qemu-rdp-bridge |
| Doc #16 is the master action plan | Consolidates all prior docs | Create new synthesis documents |
| Stop researching, start engineering | 15 strategy docs is enough | More research sessions on strategy |

---

## Approaches Rejected (Do Not Suggest Again)

| Rejected Approach | Why Rejected |
|-------------------|--------------|
| Employee/revenue threshold for free tier | Unverifiable, awkward to enforce, not correlated with usage |
| Perpetual licenses | Creates perverse incentive with BSL Change Date |
| Per-server flat rate at scale | Rewards large deployments with near-zero costs |
| Comparing to database BSL pricing (CockroachDB, MariaDB, Akka) | Wrong market segment; compare to remote access software |
| Technical license enforcement for v1.4.0 | Too much engineering; honor system is fine for now |
| Standalone qemu-rdp-bridge binary | Duplicates lamco-rdp-server's encoding pipeline |

---

## Current State

### COMPLETED in dev repo (~/lamco-rdp-server-dev):

- **LICENSE** — Updated Additional Use Grant (single server + non-profits). No residual "OR". Clean.
- **README.md** — Updated License section with new pricing table (Community/Personal/Team/Business/Corporate/Enterprise)
- **CONTRIBUTING.md** — Updated: "Free use is granted for single-server deployments and non-profits"
- **data/io.lamco.rdp-server.metainfo.xml** — Updated licensing description
- **packaging/flatpak/io.lamco.rdp-server.metainfo.xml** — Updated licensing description
- **packaging/debian/copyright** — Updated Additional Use Grant section. No residual "OR".

### NOT YET DONE in public repo (~/lamco-rdp-server):

These files still have OLD terms and need updating:

1. **LICENSE** — Needs complete rewrite. Public version is an older/simpler format (missing Competitive Use definitions entirely). Replace with exact copy of dev LICENSE.

2. **README.md lines 141-145** — Currently says:
   ```
   - **Free** for non-profits and small businesses (<3 employees, <$1M revenue)
   - **Commercial license:** $49.99/year or $99.00 perpetual per server
   - **Converts** to Apache License 2.0 on 2028-12-31
   ```
   Replace with:
   ```
   **Free** for personal use (single server instance) and non-profit organizations.

   | Plan | Price | Servers |
   |------|-------|---------|
   | Community | Free | 1 |
   | Personal | $4.99/mo or $49/yr | 1 |
   | Team | $149/yr | Up to 5 |
   | Business | $499/yr | Up to 25 |
   | Corporate | $1,499/yr | Up to 100 |
   | Enterprise | Custom | Unlimited |

   **Converts** to Apache License 2.0 on 2028-12-31.

   See [lamco.ai](https://www.lamco.ai) for full pricing and licensing details.
   ```

3. **CONTRIBUTING.md line 12** — Currently: "Free use is granted for non-profits and small businesses (see LICENSE for details)"
   Replace with: "Free use is granted for single-server deployments and non-profits (see LICENSE for details)"

4. **packaging/flatpak/io.lamco.rdp-server.metainfo.xml lines 33-35** — Currently:
   ```
   Free for personal use, non-profits, and small businesses (3 or fewer
   employees AND less than $1M revenue). Commercial use requires a license.
   Converts to Apache-2.0 on December 31, 2028.
   ```
   Replace with:
   ```
   Free for personal use (single server instance) and non-profit organizations.
   Commercial use requires a license starting at $4.99/month. Converts to
   Apache-2.0 on December 31, 2028.
   ```

### NOT YET DONE — Website change document for ~/lamco-admin:

Create an exhaustive document the user can hand to their website manager listing every change needed across lamco.ai. Below is the full inventory gathered by fetching every relevant page:

#### Page 1: lamco.ai (Homepage)

Current text references:
- "Free for personal use, commercial licenses available"
- "commercial licenses available" starting at "$4.99/month"

Changes needed:
- Update any free tier description to: "Free for personal use (single server) and non-profits"
- Pricing starting point ($4.99/month) can stay — that tier is unchanged

#### Page 2: lamco.ai/pricing/

This is the MAJOR page. Current tiers on this page:

OLD Free tier text:
- "Personal and home use"
- "Non-profit organizations"
- "Small businesses (≤3 employees)"
- "Companies under $1M annual revenue"
- "Educational and research use"
- "Evaluation and testing"
- "No registration. No feature limits. No time limits."

NEW Free tier (rename to "Community"):
- "Single server instance"
- "Non-profit organizations"
- "Educational and research use"
- "Evaluation and testing"
- "No registration. No feature limits. No time limits."
- REMOVE: "Small businesses (≤3 employees)" line
- REMOVE: "Companies under $1M annual revenue" line

OLD commercial threshold text: "More than 3 employees AND more than $1 million in annual revenue"
NEW commercial threshold text: "More than one server instance"

OLD tiers to REMOVE entirely:
- Perpetual ($99, 10 servers) — REMOVE
- Corporate ($599, 100 servers) — REMOVE
- Service Provider ($2,999, unlimited) — REMOVE

OLD tiers to KEEP (unchanged):
- Monthly ($4.99/mo, 1 server) — rename to "Personal Monthly"
- Annual ($49/yr) — BUT change from 5 servers to 1 server, rename to "Personal Annual"

NEW tiers to ADD:
- Team: $149/yr, up to 5 servers
- Business: $499/yr, up to 25 servers
- Corporate: $1,499/yr, up to 100 servers
- Enterprise: Custom pricing, unlimited servers, "Contact sales"

REMOVE all "Valid until Dec 31, 2028" text from tier descriptions (this was for perpetual licenses which no longer exist). The BSL conversion date should only appear in the licensing terms section, not on individual tiers.

REMOVE "One-time payment" language — all paid tiers are now annual subscriptions.

Voluntary support/donation section: unchanged.

#### Page 3: lamco.ai/products/lamco-rdp-server/

Current text references:
- "3 or fewer employees AND less than $1M revenue"
- "Commercial licenses required only for organizations with more than 3 employees AND more than $1M annual revenue"
- Pricing table: Monthly $4.99/1, Annual $49/5, Perpetual $99/10, Corporate $599/100, Service Provider $2999/unlimited

Changes needed:
- Replace free tier description with: "Free for single-server deployments and non-profit organizations"
- Replace commercial threshold with: "Commercial license required for multi-server deployments"
- Replace pricing table with new tiers (Community/Personal/Team/Business/Corporate/Enterprise)
- Remove all perpetual pricing references

#### Page 4: lamco.ai/download/

Current text: "Commercial license required for organizations with more than 3 employees AND more than $1M annual revenue"

Change to: "Commercial license required for multi-server deployments. Free for single-server use and non-profits."

#### Page 5: lamco.ai/open-source/

Current text: "Free for non-commercial use" (describing lamco-rdp-server)

Change to: "Free for single-server use and non-profits" or "Free community tier available"

#### Summary of changes across ALL pages:

| Old text (find everywhere) | New text |
|---|---|
| "3 or fewer employees" / "≤3 employees" | Remove — no longer relevant |
| "$1M revenue" / "$1,000,000" / "$1 million" | Remove — no longer relevant |
| "small businesses" (in licensing context) | "single-server deployments" |
| "Perpetual" tier ($99/10 servers) | Remove entirely |
| "Corporate" tier ($599/100 servers) | Replace with $1,499/yr/100 servers |
| "Service Provider" tier ($2,999/unlimited) | Replace with "Enterprise — Custom pricing" |
| "Annual" ($49/yr, 5 servers) | Change to 1 server (Personal Annual) |
| "one-time payment" / "one-time" | Remove — all tiers are subscriptions now |
| "Valid until Dec 31, 2028" on tier cards | Remove from tier cards (keep only in BSL explanation) |
| "$49.99/year or $99.00 perpetual per server" | Remove — outdated format |

---

## Authoritative License Text

The dev repo LICENSE (~/lamco-rdp-server-dev/LICENSE) is the authoritative source. Key Parameters section:

```
Additional Use Grant: You may make production use of the Licensed Work,
                      provided Your use does not include a Competitive Use
                      (as defined below).

                      In addition, You may make production use of the
                      Licensed Work without purchasing a commercial license
                      if any of the following apply:

                      1. You are a non-profit organization.
                      2. Your use is limited to a single server instance.

                      All other production use requires a commercial license
                      from Lamco Development. Visit https://lamco.ai for
                      pricing, or contact office@lamco.io.

                      A "Competitive Use" means using the Licensed Work to
                      create, offer, host, or distribute a Competitive
                      Product. A "Competitive Product" is any product or
                      service made available to third parties that provides
                      RDP server, remote desktop, virtual desktop
                      infrastructure (VDI), or remote access functionality
                      where such functionality is substantially derived from
                      the Licensed Work.

                      The following are not Competitive Uses:

                      (a) Using the Licensed Work to provide managed
                          services, cloud hosting, or IT infrastructure
                          where the Licensed Work is a tool used in
                          delivering those services rather than the basis
                          of a competing product.
                      (b) Using the Licensed Work for internal purposes
                          within Your organization, including use by Your
                          employees, contractors, and affiliates under
                          common control.
                      (c) Using the Licensed Work for non-commercial
                          education or research.
                      (d) Redistributing the Licensed Work in unmodified
                          source form with this License intact.

Change Date:          2028-12-31

Change License:       Apache License 2.0
```

---

## New Pricing Table

| Plan | Price | Servers |
|------|-------|---------|
| Community | Free | 1 |
| Personal | $4.99/mo or $49/yr | 1 |
| Team | $149/yr | Up to 5 |
| Business | $499/yr | Up to 25 |
| Corporate | $1,499/yr | Up to 100 |
| Enterprise | Custom | Unlimited |

---

## Priority Stack (from Doc #16, still current)

```
P0: Ship v1.4.0 (VM testing gate)
P1: D-Bus signals → CLI tool → IronRDP PR merges
P2: ClearCodec → RDPEDISP → lamco-vdi gap → Gateway JWT → v2.0 licensing decision
P3: RDP client → UDP → license keys → QEMU D-Bus mode
P4: Proxmox outreach → Devolutions conversation → RHEL/SLES docs (trigger-based)
```

---

## Session History (Brief)

| Event | What Happened |
|-------|---------------|
| Previous sessions | Licensing analysis complete: market comparables gathered, new tier structure designed |
| This session start | Dev repo LICENSE already updated from previous session |
| Dev repo updates | README, CONTRIBUTING, metainfo (x2), debian/copyright all updated successfully |
| "OR" fix | User caught residual "OR" in numbered list; fixed to period in LICENSE and debian/copyright |
| Public repo attempts | Edit tool errored repeatedly trying to update public repo files (~1 hour of retries) |
| Checkpoint | User requested checkpoint to continue in new session |
