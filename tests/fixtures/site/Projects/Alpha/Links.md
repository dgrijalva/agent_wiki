# Links

Relative page: [beta](../Beta/shared_name.md)
Same folder, encoded: [same](Shared%20Name.md)
Same folder, angle: [angle](<Shared Name.md>)
Absolute page: [home](/Home.md)
With fragment: [frag](/Home.md#home-heading)
Folder: [beta folder](../Beta/)
External: [ext](https://example.com/x.md)
Mail: [mail](mailto:someone@example.com)
Anchor: [anchor](#links)
Relative image: ![diagram](diagram.png)
Absolute image: ![logo](/assets/logo.svg)
Embed relative: ![[diagram.png]]
Embed absolute with alt: ![[/assets/logo.svg|Site logo]]
Escape link: [escape](../../../outside.md)
Escape image: ![escape](../../../../etc/passwd.png)
Escape embed: ![[../../../x.png]]
Absolute escape: [abs escape](/../Home.md)
Inside-root dotdot: [ok](../../Home.md)
Undecodable link: [bad bytes](../../../../admin%FF)
Undecodable image: ![bad image](x%FF/../../../../api/logout)
Undecodable embed: ![[../../../%FF.png]]
Query only: [query](?x=1)
Note embed stays text: ![[Home]]
Tag after embed: ![[diagram.png]] #linktest
