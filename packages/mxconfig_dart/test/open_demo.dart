// Opens the generated client on the standard source. Run by generated_test.
import 'dart:io';

import '../example/generated/demo.dart';

Future<void> main() async {
  final demo = await Demo.open();
  await demo.setNickname('aux');
  stdout.writeln(demo.colors.accent);
  await stdout.flush();
  exit(0);
}
