; Hooks del instalador de Windows (NSIS) de cheka.
;
; La configuración del sistema (Apache, PHP, MariaDB, DNS…) la hace la propia UI la primera
; vez que se abre, como el usuario: así cheka sabe de quién son los proyectos. El instalador
; solo actualiza una configuración que ya existe y la revierte al desinstalar.

!macro NSIS_HOOK_POSTINSTALL
  ; Actualización: si cheka ya configuró este equipo, se reconfigura con la versión nueva
  ; (el dueño de los proyectos está en etc\user, así que no importa que esto corra elevado).
  IfFileExists "$COMMONAPPDATA\cheka\etc\user" 0 cheka_post_done
    DetailPrint "Actualizando la configuración de cheka…"
    nsExec::Exec '"$INSTDIR\cheka.exe" install'
    Pop $0
    StrCmp $0 "0" cheka_post_done
      DetailPrint "No pude actualizar cheka (código $0): abre cheka y usa Actualizar."
  cheka_post_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Quita servicios, regla DNS, bloque del archivo hosts y PATH. Conserva tus proyectos,
  ; MariaDB (con tus bases de datos) y C:\ProgramData\cheka (PHP y Apache descargados).
  IfFileExists "$COMMONAPPDATA\cheka\etc\user" 0 cheka_pre_done
    DetailPrint "Quitando los servicios de cheka…"
    nsExec::Exec '"$INSTDIR\cheka.exe" uninstall'
    Pop $0
  cheka_pre_done:
!macroend
