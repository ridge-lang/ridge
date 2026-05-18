fn io log (msg: Text) -> Unit = Io.println msg

pub fn io fs readAndPrint (path: Text) -> Result Unit Text =
    let content = Fs.read path ?
    Io.println content
    Ok ()
