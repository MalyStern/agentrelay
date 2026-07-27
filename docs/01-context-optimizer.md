# AgentRelay — מודול Context Optimizer (מנוע ניהול-הקשר ותקציב)

> **מעמד:** מודול-ליבה (לא feature-לוואי). הוא מחזק ישירות את הבטחת-המוצר: *יותר עבודה בכל מכסה, פחות עצירות באמצע, פחות בזבוז על קריאה-חוזרת של אותו חומר.* משתלב עם תכנון-העבודה (§6), חיזוי-המכסה (§9), חלוקה-ליחידות (§13) וה-resume-אחרי-איפוס (§9).
>
> **לא** "כלי לקיצור פרומפטים". זהו מנוע שמחליט, בכל רגע, **מה הסוכן באמת צריך לראות** — עונה על 4 שאלות:
> 1. מה הסוכן צריך לדעת עכשיו? 2. מה כבר לא רלוונטי? 3. מה כבר נקרא ואפשר להפנות אליו במקום לשלוח שוב? 4. איזו פעולה תיתן את התוצאה הטובה ביותר ביחס למכסה שנותרה?

---

## תוכן
1. [עקרון-על ומיקום בארכיטקטורה](#core)
2. [תצוגת-המשתמש: Context Health](#health)
3. [13 היכולות — במימוש קונקרטי](#caps)
4. [מודל-הנתונים של המודול](#data)
5. [שלוש רמות-חיסכון (Safe/Adaptive/Aggressive)](#tiers)
6. [Measured מול Estimated — כלל היושרה](#honesty)
7. [סדר-בנייה בתוך המודול](#build)

---

<a name="core"></a>
## 1. עקרון-על ומיקום בארכיטקטורה

ה-Context Optimizer יושב **בין** Mission Engine/Orchestrator לבין ה-Provider Adapters. כל דבר שנשלח לסוכן עובר דרכו; כל דבר שחוזר מסוכן נרשם דרכו.

```
Orchestrator ─┐
              ├─► Context Optimizer ─► Provider Adapter ─► Claude/Codex
Scheduler   ──┘        │  ▲
                       │  │  (File Receipts · Tool-Output artifacts · 3-layer memory)
                  SQLite + artifact-store (דיסק מקומי)
```

**עקרון-היושרה של המודול (חוזר לאורך כולו):** החיסכון נעשה **במה שנשלח למודל, לא במחיקת-מידע.** הקובץ המקורי / הפלט המלא תמיד נשארים זמינים מקומית ב-artifact-store. המודל מקבל גרסה-ממוקדת + מצביע (`agentrelay://…`) לפתיחה-לפי-דרישה.

**מדוע זה מודול-ליבה ולא תוסף:** הוא מחבר שרשרת שכבר קיימת בתוכנית — resume אחרי-איפוס (§9) *בונה* על 3-שכבות-הזיכרון; מפקד/מבצע (§13) *בונה* על "Claude לא קורא כל קובץ ש-Codex קרא"; חיזוי-מכסה (§9.1) *ניזון* מ-Token Waste Detector. בלי המודול הזה, כל אלה חלשים יותר.

---

<a name="health"></a>
## 2. תצוגת-המשתמש: Context Health

בפאנל-ה-Control הימני (§3 של מסמך-האב), לכל session פעיל:

```text
Context health: 72%

Active task: Password reset API

Context currently used by:
- Relevant source files: 31%
- Test output:            18%
- Conversation history:   17%
- Tool definitions:       14%
- Repeated or stale:      20%   ⚠

Potential saving: 18K–27K input tokens

Recommended action:
Compress old test logs and start the next unit in a clean worker context.  [Apply] [Ignore]
```

- אחוזי-הקטגוריות מחושבים מ-token-accounting מקומי (ראו §4 `context_ledger`).
- ה-"Recommended action" הוא **המלצה**, לא פעולה-אוטומטית (אלא ב-Adaptive/Aggressive לפי מדיניות).
- קטגוריית "Repeated or stale" מעל סף → Token Waste Detector נדלק (§3.12).

---

<a name="caps"></a>
## 3. 13 היכולות — במימוש קונקרטי

### 3.1 מפת-ריפו מקומית — לא לשלוח את כל הריפו לכל סוכן
במקום "קרא את הפרויקט והבן אותו" (שגורם לקריאת-עשרות-קבצים לא-קשורים), AgentRelay בונה **מקומית** מפת-ריפו (ללא-מודל, זול): מבנה-תיקיות · שפות · entrypoints · imports · classes/functions · tests-קשורים · config · DB-models · API-routes · קבצים-ששונו-לאחרונה.

- **מנגנון:** ניתוח-AST מקומי (tree-sitter per-language) + git. נבנה פעם, מתעדכן אינקרמנטלית על-שינוי-קובץ. **יש כאן חפיפה ל-Graphify שכבר בשימוש שלך** — אפשר לאמץ את אותו רעיון גרף-ידע (god-nodes, imports) כבסיס-המפה. ראו שאלה-פתוחה §18.
- **בחירת-הקבצים-הראשונית ל-unit:** ה-Orchestrator שואל את המפה "אילו קבצים רלוונטיים ל'תקן איפוס-סיסמה'?" ומזין ל-worker **רק** את הקבוצה הצרה:
  ```
  src/auth/password-reset.ts · src/auth/token-service.ts · src/routes/auth.ts
  tests/password-reset.test.ts · prisma/schema.prisma
  ```
  הסוכן יכול לבקש קובץ-נוסף **כשיש סיבה** (הבקשה עוברת דרך המודול → File Receipt קודם, §3.2).
- **יתרון-ניהולי:** Claude-מפקד לא קורא כל קובץ ש-Codex קרא. Codex מחזיר Diff+סיכום+החלטות+Tests+קבצים-שנגע+סיכונים (ה-Result-Packet, §13). המפקד מקבל **הקשר-ניהולי-קטן**, לא session-מפוצץ-בקוד.

### 3.2 File Receipts — למנוע קריאה-חוזרת
בעיה מאומתת: sessions ארוכים (במיוחד אחרי compaction) קוראים שוב-ושוב אותם קבצים גדולים ([Codex #16812](https://github.com/openai/codex/issues/16812)). אחרי כל קריאת-קובץ, המודול שומר **Receipt**:
```yaml
file: src/auth/service.ts
hash: 8e217c                 # אם ה-hash זהה → הקובץ לא השתנה
read_at: 01:14:22
relevant_symbols: [login, createRefreshToken, verifyPassword]
summary:
  - Uses Argon2
  - Access token expires after 15 min
  - Refresh token stored hashed
```
כשהסוכן מבקש לקרוא שוב:
- קובץ **לא השתנה** (hash זהה) → מקבל קודם את ה-**Receipt** (עשרות טוקנים במקום אלפים).
- נדרש פרט-מסוים → נשלח **רק הטווח** הרלוונטי (סימבול/שורות).
- קריאה-מלאה **רק לפי-צורך** מפורש.
- **מימוש קונקרטי:** נאכף ב-Hook. Claude — `PreToolUse` על `Read` בודק מול טבלת-Receipts; אם hit, ה-hook מחזיר את ה-Receipt כ-context ו/או מצמצם את הבקשה. Codex — שכבת-ה-adapter מיירטת קריאות-קובץ. הקובץ המקורי תמיד זמין מקומית.

### 3.3 Tool Output Compressor — דחיסת פלט-כלים
פלטי-כלים מבזבזים ענק (2,000 שורות build · stack-trace חוזר · רשימת-כל-הקבצים · JSON ענק · test-output מלא · Docker logs). המודול שומר **פלט-מלא בדיסק** (artifact-store) ומעביר לסוכן **גרסה-ממוקדת**:
```text
Test run: FAILED   Passed: 83  Failed: 2  Skipped: 1
Unique failures:
 1. password-reset.test.ts:84 — Expected 200, received 500. Root: Missing SMTP_TEST_URL
 2. auth.integration.test.ts:112 — same missing variable
Full log: agentrelay://artifacts/test-run-1842
```
הסוכן יכול לבקש `Open lines 70–130 of test-run-1842` במקום לקבל הכל שוב.
- **מנגנון-דחיסה (דטרמיניסטי, ללא-מודל היכן שאפשר):** מזהי-פורמט (jest/pytest/tsc/eslint/docker) → parser ייעודי מחלץ pass/fail/unique-errors; dedup של stack-traces חוזרים; truncation-חכם עם מצביע. סיכום-חופשי (כשאין parser) → מודל-קטן (§3.10).

### 3.4 זיכרון בשלוש-שכבות
לא הכל צריך להישאר בשיחה:
- **שכבה א — חוזה-קבוע (תמיד נשלח):** המטרה · האיסורים · Definition-of-Done · החלטות-ארכיטקטוניות · הרשאות · אילו-פעולות-דורשות-אישור. ← זה **ה-Mission Contract** (§6) בפועל.
- **שכבה ב — מצב-המשימה (תמיד נשלח):** מה-הושלם · מה-פעיל · tests-אחרונים · קבצים-ששונו · חסמים · הצעד-הבא. ← זה **ה-Checkpoint summary** (§9.2) בפועל.
- **שכבה ג — היסטוריה-מפורטת (זמינה לחיפוש-לפי-דרישה):** שיחות-ישנות · tool-outputs · ניסיונות-שנכשלו · logs · קוד-קודם.

**רק שכבות א+ב נשלחות בכל המשך.** שכבה ג ב-artifact-store, נשלפת לפי-בקשה. זה **מנתק את התלות ב-`/compact`** (ש"עלול לאבד הוראות-מוקדמות ו-tool-outputs" — אזהרה של Claude Code עצמו) — הכללים-המתמידים חיים בשכבה-א, לא בשיחה שנדחסת.

### 3.5 Compaction חכם לפי גבולות-עבודה
במקום לחכות שהחלון כמעט-מלא, דוחסים ב**גבול-טבעי של unit**:
```text
✓ Database unit completed  ✓ Tests passed  ✓ Checkpoint saved
Context used: 64%
Recommended: Start API unit in a clean context.
```
עדיף על-פני דחיסה באמצע-תיקון-מורכב. המודול בוחר בין: להישאר-באותו-session · Compact-ממוקד (`/compact` עם הנחיית-מיקוד) · Worker-חדש · Session-חדש-מ-Checkpoint · העברת-unit-לספק-אחר. **הבחירה מתועדת כאירוע** ומקושרת ל-Checkpoint (§9) — ככה resume-אחרי-דחיסה בטוח.

### 3.6 Subagents לחומר-כבד (context מבודד)
לחקור 40 קבצים כדי להבין מנגנון-legacy? לא שהמפקד יקרא הכל:
1. Worker-מחקר מקבל **context נפרד**. 2. קורא 40 קבצים. 3. מחזיר מפת-רכיבים+סיכום-מאומת. 4. **רק הסיכום** נכנס ל-context הראשי.
זהו בדיוק היתרון ש-Claude Code מתאר ל-subagents (חלון-הקשר-נפרד, רק-סיכום-חוזר). אוטומטי עבור: מחקר-ריפו · סקירת-logs · קריאת-docs · חיפוש-שימושים · בדיקת-tests · ניתוח-dependency. **משתלב עם §13** — worker-מחקר הוא סתם עוד תפקיד ב-Orchestrator.

### 3.7 Prompt Compiler — הסרת-בזבוז לפני-שליחה
מנתח הוראות לפני-שליחה ומסיר: כלל-שחוזר-3-פעמים · מידע-שכבר-ב-Contract · הסברים-ארוכים-מיותרים · tool-descriptions-לא-רלוונטיים · תאריך/שעה-מוזרקים · היסטוריה-שלא-משפיעה-על-ה-unit. במקום 4,000 טוקנים של "אתה עובד בפרויקט… חשוב שתזכור… כפי שהוסבר קודם…", ה-worker מקבל **Work-Packet קצר**:
```yaml
objective: Implement password reset endpoint
constraints: [Do not modify UI, Do not install dependencies]
allowed_paths: [src/auth/**, tests/auth/**]
validation: [npm test -- password-reset, npm run typecheck]
references: [decision://auth-token-format, receipt://src/auth/service.ts]
```
ה-`references://` הם מצביעים ל-artifact-store — לא התוכן עצמו.

### 3.8 טעינת-כלים-לפי-צורך
הרבה MCP-tools/plugins ממלאים context עוד לפני שהמשתמש ביקש. המודול מפעיל **רק כלים-רלוונטיים**: קוד-מקומי→Git/Files/Tests · בדיקת-PR→GitHub · תזכורת→Scheduler · בית-חכם→Home-Assistant **רק-בסיום** · מחקר→Web **רק-לעובד-מחקר**.
- **דרך-API:** ניצול **deferred/lazy tool-loading** של Claude (הגדרות-כלים-לא-נדרשים לא מעמיסות את ה-prefix הקבוע ולא שוברות prompt-cache).
- **דרך-CLI (שליטה-חלקית):** בחירת **profile-MCP לפי-משימה** · השבתת-servers-לא-נחוצים · צמצום-skills-נשלחים · פתיחת-worker עם tool-set קטן.

### 3.9 שימוש-נכון ב-Prompt Caching
כשמריצים דרך-API, בונים את הפרומפט לפי **יציבות** (סטטי→בסוף-משתנה):
```text
[Static/reusable]  operating-rules · Mission-contract · tool-defs · architecture · standards
[Slow-changing]    verified-decisions · completed-work
[Dynamic]          current-task · latest-diff · new-error · user-message
```
Anthropic מאפשרת cache של prefix-קבוע (5דק/שעה) הכולל system/messages/docs/tools; OpenAI ממליצה אף היא סטטי-בהתחלה-משתנה-בסוף. **המבנה הזה חייב להתלכד עם 3-שכבות-הזיכרון (§3.4)** — שכבה-א/ב = החלק-הנשמר-ב-cache.

### 3.10 ניתוב-לפי-מודל (Model Routing)
לא כל משימה צריכה את המודל-החזק: מודל-קטן ל-{סיווג-קבצים, סיכום-logs, יצירת-Receipt} · מודל-חזק ל-תכנון-ארכיטקטורה · Codex ל-מימוש · Claude ל-סקירה-מורכבת.
```text
Task: Summarize test output          → tier: lightweight
Task: Decide DB migration strategy   → tier: advanced reasoning
```
מדיניות-משתמש: **Quality-first / Balanced / Save-quota / Custom.** **גבול-בטיחות:** המערכת **לא** מחליפה-מודל באמצע-החלטה-חשובה בלי מדיניות/אישור.

### 3.11 Output Budget
תשובות-מנופחות ("I've successfully completed… Here is a detailed summary… As requested…") צורכות טוקנים. לכל תפקיד **פורמט-קצר**. Worker מחזיר רק:
```json
{"status":"completed","changedFiles":4,"validation":"passed",
 "decisions":["Used existing token service"],"risks":[]}
```
הסבר-מפורט נוצר **רק** בדוח-הסופי או לפי-בקשת-משתמש. (זהה בצורתו ל-Result-Packet §13 — התלכדות מכוונת.)

### 3.12 Token Waste Detector
לומד לזהות התנהגויות-מבזבזות ומתערב: אותו-קובץ-נקרא-שוב-ללא-שינוי · אותה-פקודה-נכשלה-שוב-אותה-שגיאה · הורץ-test-suite-מלא-במקום-בודד · הסוכן-מסביר-שוב-את-התוכנית-במקום-לעבוד · נשלחו-tools-לא-בשימוש · קובץ-גדול-נטען-בשביל-פונקציה-אחת · שני-agents-קוראים-אותו-חומר-בנפרד · compaction-גרם-לקריאה-חוזרת · worker-קיבל-יותר-context-מהנדרש.
```text
Token waste detected
Codex read api-schema.json four times. The file has not changed.
Action: Created a persistent schema receipt and restricted future reads to requested sections.
```
- **מנגנון:** רץ על זרם-ה-`AgentEvent` (§5). מזהה-דפוסים דטרמיניסטי (hash-repeat, error-repeat, tool-unused). מזין את חיזוי-המכסה (§9.1) ואת ה-Savings-Dashboard.

### 3.13 Savings Dashboard
מראה ערך-מוחשי — **עם סימון-מקור לכל מספר:**
```text
This mission
Estimated tokens avoided:      184K–237K      (Estimated)
Repeated file reads prevented: 31             (Measured)
Tool output compressed:        2.8 MB → 34 KB (Measured)
Full test runs avoided:        6              (Measured)
Clean worker sessions created: 4              (Measured)
Prompt-cache hit rate:         81%            (Provider — when API)
Quota interruptions prevented: 1              (Estimated)
```

---

<a name="data"></a>
## 4. מודל-הנתונים של המודול (תוספת לסכמה §4 של מסמך-האב)

```sql
CREATE TABLE file_receipts (
  id TEXT PRIMARY KEY, mission_id TEXT, session_id TEXT,
  path TEXT, content_hash TEXT,          -- זהה-hash ⇒ לא-השתנה
  read_at_utc TEXT,
  relevant_symbols TEXT,                 -- JSON array
  summary TEXT,                          -- סיכום קצר
  ranges_sent TEXT                       -- אילו טווחים כבר נשלחו
);
CREATE INDEX idx_receipts ON file_receipts(session_id, path, content_hash);

CREATE TABLE artifacts (                 -- artifact-store: פלט-מלא בדיסק
  id TEXT PRIMARY KEY,                    -- agentrelay://artifacts/<id>
  mission_id TEXT, kind TEXT,             -- test_run|build|log|json|stacktrace|repo_map
  path_on_disk TEXT, size_bytes INTEGER,
  compressed_summary TEXT,               -- מה שנשלח לסוכן
  created_utc TEXT
);

CREATE TABLE context_ledger (            -- חשבונאות-טוקנים → Context Health
  id INTEGER PRIMARY KEY, session_id TEXT, ts_utc TEXT,
  category TEXT,                         -- source_files|test_output|history|tool_defs|stale
  tokens_est INTEGER
);

CREATE TABLE repo_map (                  -- מפת-ריפו מקומית
  id INTEGER PRIMARY KEY, project_dir TEXT, path TEXT,
  lang TEXT, symbols TEXT, imports TEXT, kind TEXT,   -- entrypoint|route|model|test|config
  last_modified TEXT, indexed_utc TEXT
);

CREATE TABLE savings (                   -- Savings Dashboard (עם provenance)
  id INTEGER PRIMARY KEY, mission_id TEXT, metric TEXT,
  value REAL, unit TEXT,
  provenance TEXT                        -- measured|estimated|provider
);

CREATE TABLE waste_events (
  id INTEGER PRIMARY KEY, mission_id TEXT, ts_utc TEXT,
  pattern TEXT,                          -- repeat_read|repeat_fail|full_suite|replan|unused_tools…
  detail TEXT, action_taken TEXT, tokens_saved_est INTEGER
);
```
**אירועים חדשים (`events.kind`):** `ctx.receipt_hit` · `ctx.output_compressed` · `ctx.compact_at_boundary` · `ctx.subagent_summary` · `ctx.waste_detected` · `ctx.model_routed`.

---

<a name="tiers"></a>
## 5. שלוש רמות-חיסכון

מדיניות פר-משימה (ב-Mission Contract, שדה חדש `context_policy`):

| רמה | מה כלול | ברירת-מחדל? |
|-----|---------|-------------|
| **Safe** | דחיסת-logs · מניעת-קריאות-כפולות · context-ממוקד · checkpoints · כלים-לפי-צורך. **אין הסרה של החלטות/דרישות.** | ✅ **כן** |
| **Adaptive** | + session-חדש-בין-units · workers-קטנים · ניתוב-למודל-חסכוני · סיכום-אוטומטי · הרצת-tests-ממוקדת | לפי-בחירה |
| **Aggressive** | + output-budget-מחמיר · מודלים-קטנים-למשימות-עזר · context-מינימלי · דחיסה-מוקדמת · דרישת-אישור-לפני-קריאת-קבצים-גדולים | ⛔ לא-ברירת-מחדל |

**למה Aggressive לא ברירת-מחדל:** חיסכון-מוגזם גורם לטעויות ותיקונים שיצרכו בסוף **יותר** טוקנים. עקרון: לחסוך עד-הנקודה-שבה-האיכות-מתחילה-להיפגע, לא מעבר.

---

<a name="honesty"></a>
## 6. Measured מול Estimated — כלל-היושרה

המוצר **לא ממציא נתונים.** כל מספר ב-Savings-Dashboard נושא `provenance`:
- **Measured** — נמדד ישירות (קריאות-כפולות-שנמנעו, בייטים-שנדחסו, test-runs-שנחסכו).
- **Estimated** — הערכה מודלית (טוקנים-שנחסכו-בערך, quota-interruptions-שנמנעו).
- **Provider** — רק כשמגיע מ-API רשמי (prompt-cache hit-rate).

**שתי קטגוריות נפרדות במפורש:**
- **Context saved** — הערכה לכמות-המידע-שלא-הוזן-מחדש (תמיד זמין).
- **API cost saved** — **רק** כשמתקבלים נתוני-cache רשמיים מה-API.

ב-Claude-Code ובמנויי-Codex, AgentRelay **לא שולט בהכרח בחיוב/cache-הפנימי** של הספק. לכן לא נטען שחסכנו-כסף כשאין ראיה. זה עמוד-שדרה של אמון-המשתמש (תואם §1: "כל מצב נובע ממצב-אמת").

---

<a name="build"></a>
## 7. סדר-בנייה בתוך המודול

**הפיצ'ר-הראשון (החלטת-המשתמש):** **Tool Output Compressor + File Receipts.** למה דווקא הם — ניתנים-למדידה · עובדים עם Claude **וגם** Codex · לא-תלויי-מודל · פותרים בזבוז-גדול בלי-לסכן-איכות.

1. **Phase 1 (עם ה-MVP):** File Receipts + Tool Output Compressor + artifact-store + context_ledger בסיסי (Context Health). ← מתחבר ישר ל-resume: 3-שכבות-הזיכרון = מנגנון-ה-Checkpoint.
2. **Phase 2:** Prompt Compiler · Compaction-לפי-גבול · Token Waste Detector · Savings Dashboard (Measured בלבד בהתחלה).
3. **Phase 3:** מפת-ריפו (או אימוץ-Graphify) · Subagents-לחומר-כבד · Model Routing · Output Budget פר-תפקיד.
4. **Phase 4 (כשמריצים דרך-API):** Prompt-caching מובנה · deferred-tool-loading · Provider cost-saved אמיתי.

**קריטריון-הצלחה של המודול:** באותה משימת-לילה של ה-MVP — Codex לא קורא את `api-schema.json` פעמיים; test-output של 2,000 שורות מגיע לסוכן כ-30 שורות + מצביע; ובדוח-הבוקר יש שורת "Context saved" **מסומנת נכון** Measured/Estimated.

---

*מקורות:* [Codex repeated-reads #16812](https://github.com/openai/codex/issues/16812) · [Claude Code — how it works (compaction warning)](https://code.claude.com/docs/en/how-claude-code-works) · [Claude sessions /compact /clear](https://code.claude.com/docs/en/sessions) · [Claude context-window & subagents](https://code.claude.com/docs/en/context-window) · [Claude tool-use + prompt-caching](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-use-with-prompt-caching) · [Claude prompt-caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching) · [OpenAI — Codex agent loop / caching](https://openai.com/index/unrolling-the-codex-agent-loop/)
