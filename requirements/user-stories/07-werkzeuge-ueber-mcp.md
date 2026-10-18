# Werkzeuge über MCP

Als Nutzer möchte ich Fenster- und Systemfunktionen über einen integrierten MCP-Server der Sprachsitzung geben, damit die KI tun kann, was die Oberfläche tut, ohne einen schweren Zusatzprozess.

## Akzeptanz

- Die Sprachsitzung nutzt die OpenAI Realtime API.
- Die Werkzeuge hängen an einem MCP-Server, der zur Shell gehört.
- Der Server läuft integriert, nicht als eigener Prozess auf einer Node-Laufzeitumgebung.
- Er kann mindestens: einen Shell-Befehl ausführen, zwischen Einträgen wechseln, WLAN ein- und ausschalten, eine WLAN-Verbindung einrichten, eine Bluetooth-Verbindung einrichten.
- Die Sprachsitzung kann diese Werkzeuge aufrufen.
