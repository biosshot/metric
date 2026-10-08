#!/usr/bin/env python3
"""Emit a disposable mongosh probe of the shared title-search corpus.

Pipe stdout into mongosh on the task-owned Docker Mongo fixture. This generates
only internally named synthetic data; it does not change the Mongo adapter.
"""
from pathlib import Path
import json
import uuid

root = Path(__file__).resolve().parents[1]
corpus = json.loads((root / "crates/testkit/fixtures/issue-title-search.json").read_text(encoding="utf-8"))
database = "metric_pg_title_" + uuid.uuid4().hex
print("const corpus = " + json.dumps(corpus, ensure_ascii=True) + ";")
print("const fixture = db.getSiblingDB(" + json.dumps(database) + ");")
print("""
try {
    fixture.issues.insertMany(corpus.titles.map(x => ({_id:x.id,p:x.project,t:x.title})));
    fixture.issues.createIndex({t:'text'},{default_language:'none'});
    for (const query of corpus.queries) {
        const actual = fixture.issues.find({p:1,$text:{$search:query.text,$language:'none'}},{_id:1})
            .sort({_id:1}).toArray().map(x=>x._id);
        if (JSON.stringify(actual)!==JSON.stringify(query.expected)) {
            throw new Error('shared synthetic title-search corpus mismatch: '+JSON.stringify(query.text));
        }
    }
    print('Shared Mongo title-search corpus: '+corpus.queries.length+' passed');
} finally { fixture.dropDatabase(); }
""")
