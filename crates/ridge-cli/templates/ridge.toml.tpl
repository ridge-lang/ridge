[workspace]
name = "{NAME}"
version = "0.1.0"
members = ["."]

[project]
name = "{NAME}"
version = "0.1.0"
kind = "app"
entry = "src/Main.rg"

[project.src]
root = "src"

[capabilities]
allow = ["io"]
