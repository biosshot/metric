#!/usr/bin/env python3
"""Emit an isolated mongosh reproduction of the current Session rebuild range.

This reproduces the source-selection/delete/insert algorithm, not the Rust port
call. Only the internally generated fixture database is written or removed.
"""
import json
import uuid

database = "metric_pg_session_range_" + uuid.uuid4().hex
print("const fixture = db.getSiblingDB(" + json.dumps(database) + ");")
print("""
try {
    const hour = 3600000;
    const from = new Date(hour / 2), until = new Date(hour * 1.5);
    fixture.sessions.insertMany([500000, 2000000, 4000000, 6000000]
        .map((time, id) => ({_id:id,p:1,s:new Date(time)})));
    fixture.session_stats_hourly.insertMany([0,hour]
        .map(time => ({_id:time,p:1,h:new Date(time),n:NumberLong('2')})));
    const sources = fixture.sessions.find({p:1,s:{$gte:from,$lt:until}}).toArray();
    const buckets = new Map();
    for (const source of sources) {
        const time = Math.floor(source.s.getTime()/hour)*hour;
        buckets.set(time,(buckets.get(time)||0)+1);
    }
    // These are the same raw bounds and operation order as rebuild_inner.
    fixture.session_stats_hourly.deleteMany({p:1,h:{$gte:from,$lt:until}});
    let duplicate = false;
    try {
        fixture.session_stats_hourly.insertMany([...buckets]
            .map(([time,count]) => ({_id:time,p:1,h:new Date(time),n:NumberLong(String(count))})),
            {ordered:false});
    } catch (error) { duplicate = error.code === 11000; }
    const actual = fixture.session_stats_hourly.find({p:1}).sort({h:1}).toArray()
        .map(row => Number(row.n));
    if (!duplicate || JSON.stringify(actual)!==JSON.stringify([2,1])) {
        throw new Error('synthetic Session rebuild range behavior differs from source analysis');
    }
    print('Mongo Session rebuild range algorithm: duplicate key confirmed; whole-hour counts [2,2] became [2,1]');
} finally { fixture.dropDatabase(); }
""")
