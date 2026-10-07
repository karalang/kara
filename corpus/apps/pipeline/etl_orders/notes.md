lines: 345
build: ok
interp: same
mirror: agrees
stmt-par: L285-L286 two fs.write calls (customers.csv, orders.csv); L288-L289 load_orders("orders.csv") / load_customers("customers.csv"), each an fs.read_to_string plus parse of a separate file
shared types: none
workarounds: none (fixes made were ordinary spec-level errors, not compiler failures: `&&`/`||` rewritten as `and`/`or` per the parser diagnostic; added the spec'd call-site `mut` marker on the two `add_to(mut by_tier..., ...)` calls)
ignored diagnostics: 3 (`error[borrow_projection_copy]` at L296 moving for-element `c` into the Map, and L336/L341 moving for-element `key` into pad_right)
