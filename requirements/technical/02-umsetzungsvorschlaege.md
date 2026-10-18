# Umsetzungsvorschläge

Wege aus der Konzeptarbeit. Keine Anforderungen. Jeder Punkt kann entfallen, ohne dass die User Stories ihre Geltung verlieren.

- Der Compositor zeichnet in Software, zeigt immer eine Vollbildfläche und animiert nicht.
- Der Browser bleibt ein eigenes Programm im Vollbild. Die Shell besitzt die Leiste. Eine Einbettung der Browser-Engine nimmt den Speicher der Seite nicht weg und ist deshalb kein Ziel für sich.
- Firefox mit eng begrenzter Prozesszahl ist der Ausgangspunkt, weil eine Rust-Shell sich damit nicht als Bibliothek verbinden lässt und Chromium auf 1 GB meist schwerer ist. Chromium bleibt zulässig, wenn es sich auf diesem Speicher leichter betreiben lässt.
- In der Leiste bleiben auch nicht gewählte Einträge sichtbar. Ihr Web-Dokument wird entladen, bevor die Platte übernimmt. Erneutes Wählen lädt die Seite neu. Eingaben in einem entladenen Eintrag bleiben nicht erhalten.
- Das Startbild zeichnet die Shell, ohne den Browser dafür zu starten.
- Sprachclient und MCP-Server leben im Prozess der Shell.
- Die Leiste klappt erst nach kurzer Verzögerung auf, damit ein Vorbeiziehen des Zeigers sie nicht aufblättert.
- Während ein Video läuft, ist die Leiste verborgen. Sie kommt wieder, wenn der Zeiger den linken Rand berührt.
- Unten in der Leiste sitzen feste Symbole für Netz, Ton, Ausschalten und Sprache. Nur der mittlere Bereich scrollt.
- Die Fn-Tasten für Helligkeit, Lautstärke, Stumm, Ruhezustand, Anzeige und Touchpad bleiben ein direkter Weg und gelten ohne API.
- Shell-Befehle, die das System verändern oder weit reichen, brauchen eine sichtbare Grenze, bevor sie laufen.
- Die baubare Reihenfolge ist: Leiste, Startbild und WLAN mit der Maus; danach der Browser als Vollbild; danach die Sprachsitzung; danach die übrigen Werkzeuge.
