# Native menus

Native held-menu actions carry a unique request, original row/source and offered
verbs through Swift and the C bridge. A response cannot act on a newer menu.

UIKit does not guarantee action/disappearance callback order: visual dismissal
removes the menu, but retains at most one correlated answer for its original row.
Selection consumes it once; another hold, navigation, source change, reconnect
or deactivation revokes it. Native disappearance alone never authorizes a verb.

The shipped Swift callback tests use inert UIKit adapters to exercise both
callback orders, duplicate responses and synchronous dismissal reentry. SDK
compilation and native qualification remain separate from those tests; they
do not establish physical-phone gesture behavior.
