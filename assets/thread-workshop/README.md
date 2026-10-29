# bobbin hero source

The approved Thread Workshop illustration family uses the same palette, frame and lettering across the Quipu stack. This card and both theme previews are unchanged from revision 2. Full family specification: [Tambo theme](https://github.com/intigraph/tambo/blob/61de384f883d2f2fcdf6b634b04dd6aa32e38af7/docs/stack/thread-workshop/THEME.md).

Reproduce with pixelsrc commit `48023226cacf81ce5ba5e493cc98d94fea5d7e23`:

```sh
pxl validate --strict assets/thread-workshop/bobbin.pxl
pxl render --strict assets/thread-workshop/bobbin.pxl --sprite bobbin --scale 2 -o assets/thread-workshop/bobbin.png
```

Display at 704 CSS pixels or less, preserving the aspect ratio. The transparent margin and opaque panel are identical in light and dark themes.
