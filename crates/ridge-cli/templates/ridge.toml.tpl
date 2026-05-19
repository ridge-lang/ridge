[workspace]
name = "{NAME}"
version = "0.1.0"
members = ["."]

[project]
name = "{NAME}"
version = "0.1.0"
kind = "app"
entry = "src/Main.ridge"

[project.src]
root = "src"

[capabilities]
allow = ["io"]
