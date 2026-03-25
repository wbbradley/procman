# Fan-out

The `for` block lives inside a job and wraps `env` and `run`. It iterates over a
typed iterable, binding a local variable per iteration — one process instance is
spawned for each element.

## Basic Example

```
job nodes {
  once = true

  for config_path in glob("configs/node-*.yaml") {
    env NODE_CONFIG = config_path
    run "start-node --config $NODE_CONFIG"
  }
}
```

This spawns one instance per matching file, each with its own `NODE_CONFIG`
environment variable.

## Iterable Types

| Syntax | Description |
|--------|-------------|
| `glob("pattern")` | File glob, evaluated at runtime (after `wait` conditions are satisfied), sorted lexicographically. Zero matches is a runtime error. |
| `["a", "b", "c"]` | Literal array of strings |
| `0..3` | Exclusive range: 0, 1, 2 |
| `0..=3` | Inclusive range: 0, 1, 2, 3 |

### `glob()`

```
job nodes {
  once = true
  for config_path in glob("configs/node-*.yaml") {
    env NODE_CONFIG = config_path
    run "start-node --config $NODE_CONFIG"
  }
}
```

### Literal array

```
job services {
  once = true
  for svc in ["auth", "billing", "notifications"] {
    env SERVICE = svc
    run "deploy-service $SERVICE"
  }
}
```

### Range

```
job workers {
  for i in 0..4 {
    env WORKER_ID = i
    run "worker --id $WORKER_ID"
  }
}
```

## Instance Naming

Instances are named `{job_name}-{index}` where the index is **0-based**. For the
`nodes` example with three glob matches, the instances are `nodes-0`, `nodes-1`,
and `nodes-2`.

## Group Completion

An `after @nodes` condition in another job's `wait` block is satisfied only when
**all** instances have exited successfully (exit code 0). This lets you gate a
downstream job on the entire fan-out group completing:

```
job nodes {
  once = true
  for config_path in glob("configs/node-*.yaml") {
    env NODE_CONFIG = config_path
    run "provision --config $NODE_CONFIG"
  }
}

job deploy {
  once = true

  wait {
    after @nodes
  }

  run "deploy-cluster"
}
```

Here `deploy` will not start until every `nodes-*` instance has completed
successfully.

## Env Inheritance

`env` bindings outside the `for` block apply to all instances. Bindings inside
are per-iteration:

```
job nodes {
  env CLUSTER = "prod"

  for config_path in glob("configs/*.yaml") {
    env NODE_CONFIG = config_path
    run "start-node --config $NODE_CONFIG --cluster $CLUSTER"
  }
}
```

All instances share `CLUSTER=prod`, but each gets its own `NODE_CONFIG`.

## Scoping

- The iteration variable is scoped to the `for` block
- It shares the local variable namespace with `var` bindings from `contains`
  conditions
- `args.x` and `@job.KEY` have distinct syntactic prefixes and cannot collide
  with bare local names
- Shadowing any existing local variable name is a parse-time error
