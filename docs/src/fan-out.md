# Fan-out (`for_each` glob)

The `for_each` field lets you spawn one process instance per glob match. This is useful when the
number of configuration files (or similar inputs) is not known at authoring time.

## YAML syntax

Add a `for_each` block to a process definition with two required sub-fields:

| Field  | Description |
|--------|-------------|
| `glob` | A glob pattern (e.g. `"/etc/nodes/*.yaml"`). |
| `as`   | The name of the variable that receives each matched path. |

```yaml
nodes:
  for_each:
    glob: "/etc/nodes/*.yaml"
    as: CONFIG_PATH
  run: node-agent --config $CONFIG_PATH
  once: true
```

## Variable substitution

For each glob match the `as` variable is:

1. Set in the instance's **environment** so the child process can read it directly.
2. Substituted into the `run` string — both `$VAR` and `${VAR}` forms are replaced with the
   matched path before the command is executed.

## Instance naming

Glob results are sorted lexicographically. Each instance is named
`{template_name}-{index}` where the index is **0-based**. For the example above, three matches
would produce processes named `nodes-0`, `nodes-1`, and `nodes-2`.

## Group completion

A `process_exited` dependency can reference the **template name** (e.g. `nodes`). The dependency
is satisfied only when **all** instances have exited successfully (exit code 0). This lets you
gate a downstream process on the entire fan-out group completing:

```yaml
nodes:
  for_each:
    glob: "/etc/nodes/*.yaml"
    as: CONFIG_PATH
  run: provision --config $CONFIG_PATH
  once: true

deploy:
  depends:
    - process_exited: nodes
  run: deploy-cluster
  once: true
```

Here `deploy` will not start until every `nodes-*` instance has completed successfully.

## Constraints

- **Zero matches is an error.** If the glob pattern matches no files, procman exits with an
  error rather than silently doing nothing.
- **`once: true` is typical.** Fan-out processes usually represent one-shot tasks (provisioning,
  migration, etc.). Without `once: true`, any instance exiting would trigger shutdown of the
  entire process group.
