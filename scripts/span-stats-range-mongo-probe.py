#!/usr/bin/env python3
"""Emit a synthetic mongosh reproduction of the current Span rebuild ranges.

This reproduces source selection, hourly deletion and incremental apply_stat;
it does not call the Rust port. Only the internally generated database is used.
"""
import json
import uuid

database = "metric_pg_span_range_" + uuid.uuid4().hex
print("const fixture = db.getSiblingDB(" + json.dumps(database) + ");")
print("""
try {
    const hour = 3600000;
    const base = Date.UTC(2026, 9, 9, 0, 0);
    const from = new Date(base + hour / 2), until = new Date(base + hour * 1.5);
    fixture.spans.insertMany([500000, 2000000, 4000000, 6000000]
        .map((time, id) => ({_id:id,p:1,t:true,o:NumberLong(String((base+time)*1000000)),d:NumberLong('1000000')})));
    fixture.span_stats_hourly.insertMany([base,base+hour]
        .map(time => ({_id:time,p:1,h:new Date(time),n:NumberLong('2'),f:NumberLong('0'),s:NumberLong('2000000'),d:[NumberLong('1000000'),NumberLong('1000000')]})));
    const actual = [];
    for (let pass = 0; pass < 2; pass++) {
        // Same raw bounds and operation order as signals.rs::rebuild_inner.
        fixture.span_stats_hourly.deleteMany({p:1,h:{$gte:from,$lt:until}});
        const sources = fixture.spans.find({p:1,t:true,o:{
            $gte:NumberLong(String(from.getTime()*1000000)),
            $lt:NumberLong(String(until.getTime()*1000000))}}).sort({o:1,_id:1}).toArray();
        for (const source of sources) {
            const time = Math.floor(Number(source.o)/1000000/hour)*hour;
            // All four spans share the same aggregate dimensions. apply_stat
            // increments a surviving bucket and creates any deleted bucket.
            fixture.span_stats_hourly.updateOne({_id:time}, {
                $setOnInsert:{p:1,h:new Date(time)},
                $inc:{n:NumberLong('1'),f:NumberLong('0'),s:source.d},
                $push:{d:{$each:[source.d],$slice:-2048}}
            }, {upsert:true});
        }
        actual.push(fixture.span_stats_hourly.find({p:1}).sort({h:1}).toArray()
            .map(row => Number(row.n)));
    }
    if (JSON.stringify(actual)!==JSON.stringify([[3,1],[4,1]])) {
        throw new Error('synthetic Span rebuild behavior differs from source analysis');
    }
    print('Mongo Span rebuild range algorithm: [2,2] -> [3,1] -> [4,1]; generated fixture removed after completion');
} finally { fixture.dropDatabase(); }
""")
