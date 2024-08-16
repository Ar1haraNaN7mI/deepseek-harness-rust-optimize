fn echo(args) {
    // Accept map {input:..}, plain string, or empty
    host_log("echo plugin invoked");
    if type_of(args) == "string" {
        return "echo: " + args;
    }
    let input = args["input"];
    if input == () {
        return "echo: (empty) cwd=" + host_cwd();
    }
    return "echo: " + input;
}
