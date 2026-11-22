// Runs the edits of CAF's patch to claude-agent-acp, as caf-core's tests
// give them on standard input: `{ version, edits: { <name>: { find, replace } } }`.
// Each edit's own code runs under "use strict" in a stand-in for the
// adapter's consumer, which declares only the names the adapter's code has
// where that edit goes, so a name an edit uses that isn't there fails here.
// Prints nothing, and exits 1 with what failed.
//
// The stand-in's scopes are mirrored by hand from acp-agent.js: when an
// edit uses a name it didn't before, check where it goes in acp-agent.js
// that the name is there, and only then declare it here.
import assert from "node:assert/strict";

const AsyncFunction = (async () => {}).constructor;
let input = "";
for await (const chunk of process.stdin) input += chunk;
const { version, edits } = JSON.parse(input);

// What an edit adds: its replacement less the text it found, before or after.
const added = (name) => {
    const { find, replace } = edits[name];
    if (replace.startsWith(find)) return replace.slice(find.length);
    if (replace.endsWith(find)) return replace.slice(0, replace.length - find.length);
    throw new Error(`${name} doesn't keep the text it finds`);
};

// The version the adapter names itself by.
const packageJson = { version: "0.98.0" };
const agentInfo = new Function("packageJson", `"use strict"; return {\n${edits["1-version"].replace}};`)(packageJson);
assert.equal(agentInfo.version, version);

// The consumer: its locals as the adapter declares them, the edits' own
// declarations among them, and each place an edit goes as a function.
const consumer = new AsyncFunction("session", "sendUpdate", `"use strict";
    const params = { sessionId: "s" };
    const attachUsageModel = (update) => ({ ...update, _meta: { ...(update._meta ?? {}), "_claude/model": "m" } });
${edits["2-scratch"].replace}
${edits["3-reset"].replace}
            heldLimits.length;
        };
${edits["5-fail"].replace}
            return "failed";
        };
    return {
        fail: failActiveWithSessionFailure,
        held: () => heldLimits.length,
        resetTurnScratch,
        rateLimit: async (message) => {
            switch (message.type) {
${edits["9-limits"].replace}
            }
        },
        compacted: async (usedTokens) => {
            lastAssistantTotalUsage = usedTokens;
${edits["6-compact"].replace}
        },
        streamed: async (nextUsage) => {
            lastAssistantTotalUsage = nextUsage;
            session.contextUsedTokens = nextUsage;
            await sendUpdate({
                sessionId: params.sessionId,
                update: attachUsageModel({
                    sessionUpdate: "usage_update",
${edits["7-stream"].replace}
        },
        replied: async (total) => {
            lastAssistantTotalUsage = total;
${edits["8-reply"].replace}
        },
        settle: async () => {
            const turn = session.activeTurn;
            if (!turn || turn.settled || turn.settling) {
                return "not settled";
            }
${edits["4-settle"].replace}
            return "settled";
        },
        died: async () => {
${added("10-died")}
            return "died";
        },
    };`);

const logged = [];
const logger = { error: (...args) => logged.push(args) };
const info = (n) => ({ rateLimitType: "five_hour", utilization: n / 10 });
const limitsOf = (sent) => sent.filter((u) => u.update._meta?.["_claude/rateLimit"]);
const start = async (session) => {
    const sent = [];
    const at = await consumer.call({ logger }, session, async (notification) => {
        sent.push(notification);
        session.onSend?.(notification);
    });
    return { at, sent };
};

// No turn active, nothing ever measured: sent at once, as the limits alone.
{
    const { at, sent } = await start({ activeTurn: null, contextWindowSize: 200 });
    await at.rateLimit({ type: "rate_limit_event", rate_limit_info: info(1) });
    assert.deepEqual(sent, [{ sessionId: "s", update: { sessionUpdate: "usage_update", used: 0, size: 200,
        _meta: { "_claude/rateLimit": info(1), "_caf/limitsOnly": true, "_claude/model": "m" } } }]);
    assert.equal(at.held(), 0);
}

// No turn active, measured before: sent at once, with the last measure.
{
    const { at, sent } = await start({ activeTurn: null, contextWindowSize: 200, contextUsedTokens: 50 });
    await at.rateLimit({ type: "rate_limit_event", rate_limit_info: info(2) });
    assert.equal(sent.length, 1);
    assert.equal(sent[0].update.used, 50);
    assert.equal(sent[0].update._meta["_caf/limitsOnly"], undefined);
}

// During a turn, before it measures: held, in order, through the turn's
// start, and sent at each kind of measure, then a later one at once.
for (const measure of ["streamed", "replied", "compacted"]) {
    const { at, sent } = await start({ activeTurn: {}, contextWindowSize: 200 });
    await at.rateLimit({ type: "rate_limit_event", rate_limit_info: info(1) });
    at.resetTurnScratch();
    await at.rateLimit({ type: "rate_limit_event", rate_limit_info: info(2) });
    assert.equal(sent.length, 0, measure);
    assert.equal(at.held(), 2, measure);
    await at[measure](70);
    await at.rateLimit({ type: "rate_limit_event", rate_limit_info: info(3) });
    assert.deepEqual(limitsOf(sent).map((u) => [u.update.used, u.update._meta["_claude/rateLimit"]]),
        [[70, info(1)], [70, info(2)], [70, info(3)]], measure);
}

// Held to the turn's end: sent before its answer, whichever way it ends.
for (const end of ["settle", "fail", "died"]) {
    const { at, sent } = await start({ activeTurn: {}, contextWindowSize: 200 });
    await at.rateLimit({ type: "rate_limit_event", rate_limit_info: info(4) });
    assert.equal(await at[end](), end === "settle" ? "settled" : end === "fail" ? "failed" : "died");
    assert.equal(limitsOf(sent).length, 1, end);
    assert.equal(sent[0].update._meta["_caf/limitsOnly"], true, end);
    assert.equal(at.held(), 0, end);
}

// A cancel that settles the turn while they're sent: the settle stops.
{
    const session = { activeTurn: {}, contextWindowSize: 200 };
    session.onSend = () => { session.activeTurn.settled = true; };
    const { at } = await start(session);
    await at.rateLimit({ type: "rate_limit_event", rate_limit_info: info(5) });
    assert.equal(await at.settle(), undefined);
}

// A send that fails is logged, and the turn still ends.
for (const end of ["settle", "fail", "died"]) {
    const session = { activeTurn: {}, contextWindowSize: 200 };
    session.onSend = () => { throw new Error("closed"); };
    const { at } = await start(session);
    await at.rateLimit({ type: "rate_limit_event", rate_limit_info: info(6) });
    logged.length = 0;
    assert.notEqual(await at[end](), undefined, end);
    assert.equal(logged.length, 1, end);
}
