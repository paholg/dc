Output docs/src/demos/demo.gif
Source demos/lib/setup.tape

Ctrl+V
Ctrl+J

Type "# This is an example project, dcex"
Ctrl+V
Ctrl+J
Sleep 0.4s


Type "# It has a simple python server and a SQL database"
Ctrl+V
Ctrl+J
Sleep 0.4s

Type "# Just by being here, we have some env vars set"
Enter
Sleep 0.4s

Type "echo $APP_URL"
Enter
Sleep 0.4s

Type "# Which we can use to access the server"
Enter
Sleep 0.4s

Type "curl $APP_URL/hello"
Enter
Wait
Sleep 0.4s

Type "curl $APP_URL/hello"
Enter
Wait
Sleep 0.4s

Type "# Let's start a new workspace"
Enter
Wait
Sleep 0.4s

Type "dc up -g fun-feature"
Sleep 0.8s
Enter
Wait
Sleep 1.2s

Ctrl+L

Type "# New url! New environment!"
Enter
Sleep 0.4s

Type "echo $APP_URL"
Enter
Wait
Sleep 0.6s

Type "# We can work here, using URLs"
Enter
Sleep 0.4s

Type "curl $APP_URL/hello"
Enter
Wait
Sleep 0.4s

Type "# And never worry about port conflicts"
Enter
Sleep 0.4s

Type "# Or we can work inside the container"
Enter
Sleep 0.4s

Type "dc x"
Enter
Wait /➜/
Sleep 0.4s

Type "# Look ma, we're containerized!"
Enter
Sleep 0.4s

Type "curl http://localhost:8080/hello"
Enter
Wait /➜/
Sleep 0.4s

Ctrl+D
Wait
Sleep 0.4s

Type "# And we can toss the whole thing once done"
Enter
Sleep 0.8s

Type "dc destroy"
Enter
Wait
Sleep 1s

