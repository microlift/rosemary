// Compare the checked-in fixture with the reporter's unmodified native capture.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';

assert(process.argv[2], 'Usage: node verify-capture.mjs /path/to/unpacked/run');
const directory = process.argv[2];
const summary = JSON.parse(fs.readFileSync(path.join(directory, 'summary.json'), 'utf8'));
const events = fs.readFileSync(path.join(directory, 'events.jsonl'), 'utf8').trim().split('\n').map(JSON.parse);
assert.equal(summary.reason, 'completed');
assert.equal(summary.completedCases, 70);
assert.equal(summary.exceptionCount, 0);
assert.equal(summary.outcomes.length, 70);
assert(events.every((event, i) => event.sequence === i + 1));
let restored = 0;
for (const outcome of summary.outcomes) {
    assert.equal(outcome.operationApplied, true);
    if (['will-enter', 'fullscreen', 'will-exit'].includes(outcome.spec.phase)) {
        assert.deepEqual(outcome.restoredFrame, outcome.preFullscreenFrame);
        restored++;
    }
    const resized = events.find(event => event.case?.id === outcome.spec.id && event.event === 'postExitResize.after500ms');
    assert.equal(resized.window.contentView.bounds.width, 640);
    assert.equal(resized.window.contentView.bounds.height, 420);
}
const fixture = fileURLToPath(new URL('../../crates/appkit/tests/fixtures/fullscreen-macos-26.5.tsv', import.meta.url));
const rows = fs.readFileSync(fixture, 'utf8').trim().split('\n').filter(line => !line.startsWith('#'));
for (const line of rows) {
    const row = line.split('\t');
    const event = events[Number(row[12]) - 1];
    assert.equal(event.case.id, Number(row[0]));
    assert.equal(event.case.style, row[1]);
    assert.equal(event.case.operation, row[2]);
    assert.equal(event.case.phase, 'fullscreen');
    assert.equal(event.window.fullscreen, 1);
    const rect = event.window.frame;
    assert.deepEqual([rect.x, rect.y, rect.width, rect.height], row.slice(8, 12).map(Number));
    assert.equal(event.window.contentView.bounds.width, rect.width);
    assert.equal(event.window.contentView.bounds.height, rect.height);
    assert.equal(event.window.autoresizedSubview.bounds.width, rect.width);
    assert.equal(event.window.autoresizedSubview.bounds.height, rect.height);
    const begin = events.find(candidate => candidate.case?.id === event.case.id && candidate.event === 'operation.begin');
    if (['burst', 'roblox-transition'].includes(row[2])) {
        assert.equal(event.iteration, Number(row[3]));
        const name = row[2] === 'burst' ? 'request.burst' : 'request.transitionFrame';
        const request = events.find(candidate => candidate.case?.id === event.case.id && candidate.event === name && candidate.iteration === event.iteration).requestedFrame;
        assert.deepEqual([request.x, request.y, request.width, request.height], row.slice(4, 8).map(Number));
    } else if (row[2] === 'content-size') {
        assert.equal(Number(row[6]), begin.window.contentView.bounds.width);
        assert.equal(Number(row[7]), begin.window.contentView.bounds.height - 31);
    } else if (row[2] === 'origin') {
        assert.equal(Number(row[4]), begin.window.frame.x + 23);
        assert.equal(Number(row[5]), begin.window.frame.y + 31);
    } else {
        const request = begin.requestedFrame;
        assert.deepEqual([request.x, request.y, request.width, request.height], row.slice(4, 8).map(Number));
    }
}
assert.equal(rows.length, 22);
assert.equal(restored, 42);
console.log(`Native capture verified: ${events.length} observations, 70 cases, 42 restorations, 70 ordinary resizes, 22 fixture rows.`);
