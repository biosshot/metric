CREATE INDEX environment_project_timeline ON metric.environments(project_id,last_seen DESC,environment_id DESC) WHERE NOT hidden;
